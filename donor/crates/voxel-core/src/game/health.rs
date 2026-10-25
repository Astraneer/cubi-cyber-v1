//! Здоровье, урон, воздух, регенерация игрока (ASP-12, Java 1.21.1): окно неуязвимости, броня, падение, утопление,
//! регенерация [наше: ½ ♥ раз в 40 тиков после 100 тиков без урона — mc/decisions.md], еда лечит. Чистые данные и
//! формулы без мира и сети: сервер (game/server.rs, `ServerPlayer.hp`) меняет их, клиент держит зеркало (`Vitals`) для
//! HUD. f32 там, где Java хранит float (здоровье, урон, fallDistance). Числа — ASP-12 §16, тесты внизу.
//!
//! Порядок тика игрока (ASP-12 §1): `begin` (счётчики −1) → опасности baseTick (пустота, стена, воздух, огонь) → движение
//! (падение, кактус) → `regen`. Окно: `time_until_regen` уменьшается ДО опасностей, иначе повторяющийся урон
//! (кактус, стена) проходил бы раз в 11 тиков, а не в 10.

pub(crate) const MAX_HP: f32 = 20.0;
/// Воздух: максимум, тиков.
pub(crate) const MAX_AIR: i32 = 300;
/// Неуязвимость после входа и возрождения, тиков (joinInvulnerabilityTicks).
pub(crate) const SPAWN_PROTECT: i32 = 60;
/// Регенерация [наше]: пауза без урона и шаг лечения (½ ♥), тиков.
pub(crate) const REGEN_DELAY: u64 = 100;
pub(crate) const REGEN_EVERY: u32 = 40;
/// Тик смерти, на котором экран смерти даёт нажать «Возродиться» (кнопки неактивны первые 20 тиков).
pub(crate) const RESPAWN_AFTER: i32 = 20;
/// Огонь: горение после лавы — 15 с; урон от горения — 1 раз в секунду (Entity.baseTick).
pub(crate) const BURN_TICKS: i32 = 300;
/// Дно мира: ниже этого y — урон пустоты (out_of_world).
pub(crate) const VOID_Y: f64 = -64.0;

/// Вид урона (damage_type). Коды — в сообщениях и событиях, только дописываются.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DamageKind {
    Fall = 0,
    InWall = 1,
    Drown = 2,
    Cactus = 3,
    OutOfWorld = 4,
    PlayerAttack = 5,
    Lava = 6,
    OnFire = 7,
    InFire = 8,
    /// Отладка («урон N»).
    Generic = 9,
    /// Удар моба (зомби, ASP-18 §5): броня снижает, отбрасывает.
    MobAttack = 10,
}

impl DamageKind {
    pub(crate) fn code(self) -> u8 {
        self as u8
    }
    pub(crate) fn from_code(c: u8) -> Option<DamageKind> {
        use DamageKind::*;
        Some(match c {
            0 => Fall,
            1 => InWall,
            2 => Drown,
            3 => Cactus,
            4 => OutOfWorld,
            5 => PlayerAttack,
            6 => Lava,
            7 => OnFire,
            8 => InFire,
            9 => Generic,
            10 => MobAttack,
            _ => return None,
        })
    }
    /// tags/damage_type/bypasses_armor: броня не снижает и не тратится (кактус, удар игрока и огонь — снижает).
    pub(crate) fn bypasses_armor(self) -> bool {
        !matches!(self, DamageKind::Cactus | DamageKind::PlayerAttack | DamageKind::MobAttack | DamageKind::Lava | DamageKind::OnFire | DamageKind::InFire)
    }
    /// bypasses_invulnerability: пустота пробивает неуязвимость входа и режим полёта.
    pub(crate) fn bypasses_invulnerability(self) -> bool {
        self == DamageKind::OutOfWorld
    }
    /// tags/damage_type/no_knockback: в нашем объёме толкает только удар игрока.
    pub(crate) fn knockback(self) -> bool {
        matches!(self, DamageKind::PlayerAttack | DamageKind::MobAttack)
    }
}

/// Java MathHelper.ceil(double): значение выше целой части — вверх, иначе целая часть.
fn jceil(x: f64) -> i32 {
    let i = x as i32;
    if x > i as f64 {
        i + 1
    } else {
        i
    }
}

/// Урон падения (LivingEntity.computeFallDamage): ceil((fallDistance − 3) · 1.0), не меньше 0.
pub(crate) fn fall_damage(fall_distance: f32) -> i32 {
    let g = fall_distance - 3.0f32;
    jceil((g * 1.0f32) as f64).max(0)
}

/// То же с множителем приземления (LivingEntity.computeFallDamage(fd, k)): стог сена — 0.2.
pub(crate) fn fall_damage_k(fall_distance: f32, k: f32) -> i32 {
    let g = fall_distance - 3.0f32;
    jceil((g * k) as f64).max(0)
}

/// Урон после брони (DamageUtil.getDamageLeft): f32, как в Java.
pub(crate) fn armor_left(dmg: f32, armor: f32, toughness: f32) -> f32 {
    let f = 2.0f32 + toughness / 4.0f32;
    let lo = armor * 0.2f32;
    let g = {
        let v = armor - dmg / f;
        if v < lo {
            lo
        } else {
            v.min(20.0f32)
        }
    };
    dmg * (1.0f32 - g / 25.0f32)
}

/// Износ каждой надетой части от прошедшего урона (PlayerEntity.damageArmor): (int)max(1, урон/4).
pub(crate) fn armor_wear(dmg: f32) -> i32 {
    (dmg / 4.0f32).max(1.0f32) as i32
}

/// Исход `Health::damage`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Hit {
    /// Полный удар (вне окна): звук, красная вспышка, отбрасывание. Добор в окне — без них.
    pub(crate) full: bool,
    /// Урон, ушедший в расчёт (полный или разница в окне) до брони.
    pub(crate) raw: f32,
    /// Снято здоровья после брони.
    pub(crate) taken: f32,
    /// Износ каждой надетой части брони (0 — не тратится).
    pub(crate) wear: i32,
    /// Здоровье стало 0.
    pub(crate) died: bool,
}

/// Состояние здоровья игрока (LivingEntity + PlayerEntity: окно, воздух, огонь, регенерация).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Health {
    pub(crate) hp: f32,
    pub(crate) max: f32,
    /// Окно: 20 при полном ударе; «> 10» — в окне (прошедший урон — только больше прошлого, разницей).
    pub(crate) time_until_regen: i32,
    /// Сырой урон последнего прошедшего удара (до брони).
    pub(crate) last_damage_taken: f32,
    /// 10 → 0: модель красная (полные удары).
    pub(crate) hurt_time: i32,
    pub(crate) air: i32,
    /// Неуязвимость после входа и возрождения.
    pub(crate) spawn_protect: i32,
    /// Тиков игрока с рождения здоровья и тик последнего прошедшего урона.
    pub(crate) now: u64,
    pub(crate) last_hurt: u64,
    pub(crate) regen_timer: u32,
    /// Тиков в состоянии «мёртв».
    pub(crate) death_time: i32,
    /// Горит: тиков осталось (15 с от лавы, 8 с от костра); тушит вода.
    pub(crate) fire: i32,
    /// damageTiltYaw: градусы относительно взгляда (меняется только уроном с отбрасыванием).
    pub(crate) tilt_yaw: f32,
}

impl Health {
    /// Новый или возрождённый: полное здоровье, воздух, 60 тиков неуязвимости.
    pub(crate) fn new() -> Health {
        Health {
            hp: MAX_HP,
            max: MAX_HP,
            time_until_regen: 0,
            last_damage_taken: 0.0,
            hurt_time: 0,
            air: MAX_AIR,
            spawn_protect: SPAWN_PROTECT,
            now: 0,
            last_hurt: 0,
            regen_timer: 0,
            death_time: 0,
            fire: 0,
            tilt_yaw: 0.0,
        }
    }

    pub(crate) fn alive(&self) -> bool {
        self.hp > 0.0
    }

    /// Начало тика (ASP-12 §1, шаг 0): окно и неуязвимость −1; счётчик тиков.
    pub(crate) fn begin(&mut self) {
        self.now += 1;
        for c in [&mut self.time_until_regen, &mut self.spawn_protect] {
            if *c > 0 {
                *c -= 1;
            }
        }
    }

    /// Приём урона (ServerPlayerEntity.damage → LivingEntity.damage, ASP-12 §2.1); armor — сумма очков брони (целая,
    /// 0…30), toughness — 0 (кожа и железо). invulnerable — режим полёта: отказ для всего, кроме пустоты. None — отказ
    /// (неуязвим, мёртв, урон не больше прошлого в окне).
    pub(crate) fn damage(&mut self, kind: DamageKind, amount: f32, armor: f32, invulnerable: bool) -> Option<Hit> {
        self.damage_t(kind, amount, armor, 0.0, invulnerable)
    }

    /// То же со стойкостью брони toughness (сумма по надетым частям: алмаз 2 на часть, сталь Mekanism 2).
    pub(crate) fn damage_t(&mut self, kind: DamageKind, amount: f32, armor: f32, toughness: f32, invulnerable: bool) -> Option<Hit> {
        if !self.alive() || !(amount > 0.0) {
            return None;
        }
        if !kind.bypasses_invulnerability() && (invulnerable || self.spawn_protect > 0) {
            return None;
        }
        let full = self.time_until_regen <= 10;
        let raw = if full {
            self.last_damage_taken = amount;
            self.time_until_regen = 20;
            self.hurt_time = 10;
            amount
        } else {
            if amount <= self.last_damage_taken {
                return None;
            }
            let d = amount - self.last_damage_taken;
            self.last_damage_taken = amount;
            d
        };
        // applyDamage: износ от пришедшего урона, затем снижение.
        let (mut left, mut wear) = (raw, 0);
        if !kind.bypasses_armor() {
            if armor > 0.0 {
                wear = armor_wear(raw);
            }
            left = armor_left(raw, armor.floor(), toughness);
        }
        let before = self.hp;
        self.hp = (self.hp - left).max(0.0);
        self.last_hurt = self.now;
        self.regen_timer = 0;
        Some(Hit { full, raw, taken: before - self.hp, wear, died: self.hp <= 0.0 })
    }

    /// Лечение (LivingEntity.heal): живой, не выше максимума.
    pub(crate) fn heal(&mut self, n: f32) {
        if self.alive() {
            self.hp = (self.hp + n).min(self.max);
        }
    }

    /// Воздух (LivingEntity.baseTick): голова в воде — −1 за тик, на −20 → 0 и урон 2 (утопление); иначе +4 до 300.
    /// Ответ — урон утопления этого тика.
    pub(crate) fn tick_air(&mut self, head_in_water: bool) -> Option<f32> {
        if head_in_water {
            self.air -= 1;
            if self.air == -20 {
                self.air = 0;
                return Some(2.0);
            }
        } else if self.air < MAX_AIR {
            self.air = (self.air + 4).min(MAX_AIR);
        }
        None
    }

    /// Горение (Entity.baseTick): тушит вода; раз в 20 тиков вне лавы — урон 1 (on_fire). Ответ — урон этого тика.
    pub(crate) fn tick_fire(&mut self, in_water: bool, in_lava: bool) -> Option<f32> {
        if self.fire <= 0 {
            return None;
        }
        if in_water {
            self.fire = 0;
            return None;
        }
        let hit = self.fire % 20 == 0 && !in_lava;
        self.fire -= 1;
        hit.then_some(1.0)
    }

    /// Поджечь не меньше чем на ticks (Entity.setOnFireForTicks).
    pub(crate) fn ignite(&mut self, ticks: i32) {
        if self.fire < ticks {
            self.fire = ticks;
        }
    }

    /// Хвост baseTick: красная вспышка модели −1; мёртвый считает death_time. true — на этом тике тело убирается
    /// (20-й тик смерти: частицы, курсор и сетка падают).
    pub(crate) fn base_tick_end(&mut self) -> bool {
        if self.hurt_time > 0 {
            self.hurt_time -= 1;
        }
        if !self.alive() {
            self.death_time += 1;
        }
        !self.alive() && self.death_time == 20
    }

    /// Регенерация [наше] (вариант A): после 100 тиков без урона — ½ ♥ раз в 40 тиков. Ответ — вылечили ли на этом тике.
    pub(crate) fn regen(&mut self) -> bool {
        if self.alive() && self.hp < self.max && self.now - self.last_hurt > REGEN_DELAY && self.fire <= 0 {
            self.regen_timer += 1;
            if self.regen_timer >= REGEN_EVERY {
                self.heal(1.0);
                self.regen_timer = 0;
                return true;
            }
        } else {
            self.regen_timer = 0;
        }
        false
    }

    /// Еду можно начать есть, только если сердца не полны [наше] — проверка в начале.
    pub(crate) fn can_eat(&self) -> bool {
        self.alive() && self.hp < self.max
    }
}

/// Своё состояние для HUD и камеры, как его видит клиент (зеркало серверного `Health`, Msg::Vitals): здоровье, воздух,
/// броня, горение, смерть; счётчики кадра (крен, мигание сердец) клиент ведёт сам по изменениям (ClientPlayerEntity.updateHealth).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Vitals {
    pub(crate) hp: f32,
    pub(crate) air: i16,
    /// Очки брони (0…30).
    pub(crate) armor: u8,
    pub(crate) burning: bool,
    pub(crate) dead: bool,
    /// Тик смерти (0…): кнопка возрождения живёт с RESPAWN_AFTER.
    pub(crate) death_time: u8,
    /// Направление последнего удара с отбрасыванием (градусы относительно взгляда).
    pub(crate) tilt: f32,
    /// От чего умер (DamageKind::code) — для текста экрана смерти; пока жив — 0.
    pub(crate) cause: u8,
}

impl Vitals {
    pub(crate) const FULL: Vitals = Vitals { hp: MAX_HP, air: MAX_AIR as i16, armor: 0, burning: false, dead: false, death_time: 0, tilt: 0.0, cause: 0 };
}

// ---------------------------------------------------------------- HUD: раскладка (чистые функции для JS)

/// Ряд из 10 сердец: [заполнение красным 0/1/2, заполнение белым «призрака» 0/1/2] по сердцам слева направо — белые
/// видны во «светлых» фазах мигания (потерянная часть). hp, ghost — ceil здоровья и белое значение (InGameHud
/// renderHealthValue).
pub(crate) fn hearts_row(hp: i32, ghost: i32) -> [[u8; 2]; 10] {
    let fill = |v: i32, l: usize| -> u8 {
        let at = 2 * l as i32;
        if at + 2 <= v {
            2
        } else if at < v {
            1
        } else {
            0
        }
    };
    std::array::from_fn(|l| [fill(hp, l), fill(ghost, l)])
}

/// Пузыри воздуха (InGameHud.renderAirBubbles): (целые, лопающиеся). v = min(air, 300); целых ceil((v − 2)·10/300),
/// лопающихся ceil(v·10/300) − целых; при air ≤ 0 — нет (0, 0).
pub(crate) fn air_bubbles(air: i32) -> (i32, i32) {
    if air <= 0 {
        return (0, 0);
    }
    let v = air.min(MAX_AIR);
    let full = ((v - 2) as f64 * 10.0 / 300.0).ceil() as i32;
    let total = (v as f64 * 10.0 / 300.0).ceil() as i32;
    (full.max(0), total - full.max(0))
}

/// Крен камеры от удара (GameRenderer.tiltViewWhenHurt), градусы: t = hurtTime − tickDelta (10 → 0 за 10 тиков); пик 14°
/// через ~1.6 тика. Ноль при t < 0.
pub(crate) fn hurt_tilt_deg(t: f32) -> f32 {
    if t < 0.0 {
        return 0.0;
    }
    let u = t / 10.0;
    -(libm::sinf(u * u * u * u * std::f32::consts::PI)) * 14.0
}

/// Крен камеры при смерти, градусы: Rz(40 − 8000/(min(deathTime + td, 20) + 200)) — от 0 до 3.64° за 1 с.
pub(crate) fn death_tilt_deg(death_time: f32) -> f32 {
    40.0 - 8000.0 / (death_time.min(20.0) + 200.0)
}

/// Делитель FOV при смерти (GameRenderer.getFov): FOV / ((1 − 500/(f + 500))·2 + 1), f = min(deathTime + td, 20).
pub(crate) fn death_fov_div(death_time: f32) -> f32 {
    let f = death_time.min(20.0);
    (1.0 - 500.0 / (f + 500.0)) * 2.0 + 1.0
}

// ---------------------------------------------------------------- тексты смерти

/// Сообщение о смерти для чата (ru): «Ник разбился» и т. п. (ASP-12 §10.2; одна строка на вид урона).
pub(crate) fn death_text(nick: &str, kind: DamageKind) -> String {
    let what = match kind {
        DamageKind::Fall => "разбился вдребезги",
        DamageKind::InWall => "погребён заживо",
        DamageKind::Drown => "утонул",
        DamageKind::Cactus => "исколот до смерти",
        DamageKind::OutOfWorld => "выпал из мира",
        DamageKind::PlayerAttack => "был убит",
        DamageKind::Lava => "решил искупаться в лаве",
        DamageKind::OnFire => "сгорел заживо",
        DamageKind::InFire => "сгорел в огне",
        DamageKind::Generic => "умер",
        DamageKind::MobAttack => "был убит зомби",
    };
    format!("{nick} {what}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h() -> Health {
        let mut h = Health::new();
        h.spawn_protect = 0;
        h
    }

    #[test]
    fn fall_damage_asp12() {
        assert_eq!(fall_damage(3.0), 0);
        assert_eq!(fall_damage(3.0000002), 1);
        assert_eq!(fall_damage(4.0), 1);
        assert_eq!(fall_damage(23.0), 20);
        // Сход с края: fd после тика приземления (ASP-12 §16).
        assert_eq!(fall_damage(2.6945615), 0);
        assert_eq!(fall_damage(3.3462703), 1);
        assert_eq!(fall_damage(9.6670666), 7);
        assert_eq!(fall_damage(21.8335056), 19);
        assert_eq!(fall_damage(23.4352360), 21);
        // Прыжок с края.
        assert_eq!(fall_damage(4.0363092), 2);
        assert_eq!(fall_damage(4.8150272), 2);
        assert_eq!(fall_damage(10.7635651), 8);
        assert_eq!(fall_damage(23.3748512), 21);
    }

    #[test]
    fn armor_asp12() {
        let near = |a: f32, b: f32| assert!((a - b).abs() < 1e-4, "{a} ≠ {b}");
        near(armor_left(6.0, 15.0, 0.0), 3.12);
        near(armor_left(9.0, 15.0, 0.0), 5.22);
        near(armor_left(1.0, 15.0, 0.0), 0.42);
        near(armor_left(1.0, 7.0, 0.0), 0.74);
        near(armor_left(6.0, 7.0, 0.0), 5.04);
        near(armor_left(4.0, 6.0, 0.0), 3.36);
        assert_eq!(armor_wear(7.99), 1);
        assert_eq!(armor_wear(8.0), 2);
        assert_eq!(armor_wear(9.0), 2);
        assert_eq!(armor_wear(1.0), 1);
    }

    /// Окно: удар 6 → HP 14; +5 тиков удар 4 → отказ; удар 9 → добор 3 (HP 11, last 9, без «полного»); +10 удар 4 → полный.
    #[test]
    fn window_asp12() {
        let mut p = h();
        p.begin(); // тик T
        let a = p.damage(DamageKind::PlayerAttack, 6.0, 0.0, false).unwrap();
        assert!(a.full && p.hp == 14.0);
        for _ in 0..5 {
            p.begin();
        }
        assert!(p.damage(DamageKind::PlayerAttack, 4.0, 0.0, false).is_none());
        assert_eq!(p.hp, 14.0);
        let top = p.damage(DamageKind::PlayerAttack, 9.0, 0.0, false).unwrap();
        assert!(!top.full);
        assert_eq!(top.taken, 3.0);
        assert_eq!(p.hp, 11.0);
        assert_eq!(p.last_damage_taken, 9.0);
        for _ in 0..5 {
            p.begin();
        }
        assert_eq!(p.time_until_regen, 10); // T + 10: окно «не больше 10» — полный удар
        let c = p.damage(DamageKind::PlayerAttack, 4.0, 0.0, false).unwrap();
        assert!(c.full);
        assert_eq!(p.hp, 7.0);
    }

    /// Кактус каждый тик с T: урон на T, T+10, T+20…, 20 HP → 0 к T+190.
    #[test]
    fn cactus_every_ten_ticks() {
        let mut p = h();
        let mut hits = Vec::new();
        for t in 0..200 {
            p.begin();
            if p.damage(DamageKind::Cactus, 1.0, 0.0, false).is_some() {
                hits.push(t);
            }
            if !p.alive() {
                break;
            }
        }
        assert_eq!(&hits[..3], &[0, 10, 20]);
        assert_eq!(hits.len(), 20);
        assert_eq!(*hits.last().unwrap(), 190);
    }

    #[test]
    fn armor_wears_only_non_bypass() {
        let mut p = h();
        let hit = p.damage(DamageKind::Cactus, 1.0, 15.0, false).unwrap();
        assert_eq!(hit.wear, 1);
        assert!((hit.taken - 0.42).abs() < 1e-4);
        let mut q = h();
        let f = q.damage(DamageKind::Fall, 7.0, 15.0, false).unwrap();
        assert_eq!((f.wear, f.taken), (0, 7.0), "падение броня не снижает и не тратится");
    }

    #[test]
    fn invulnerability() {
        let mut p = Health::new(); // 60 тиков неуязвимости
        assert!(p.damage(DamageKind::Cactus, 1.0, 0.0, false).is_none());
        assert!(p.damage(DamageKind::OutOfWorld, 4.0, 0.0, false).is_some(), "пустота пробивает");
        let mut q = h();
        assert!(q.damage(DamageKind::Fall, 5.0, 0.0, true).is_none(), "полёт неуязвим");
        for _ in 0..60 {
            p.begin();
        }
        assert_eq!(p.spawn_protect, 0);
    }

    /// Воздух: от 300 под водой — 0 на тике 300, урон 2 на тиках 320, 340…; HP 20 → 0 на 500-м тике.
    #[test]
    fn drowning_asp12() {
        let mut p = h();
        let mut first = 0;
        let mut dead_at = 0;
        for t in 1..=600 {
            p.begin();
            if let Some(d) = p.tick_air(true) {
                if first == 0 {
                    first = t;
                }
                p.damage(DamageKind::Drown, d, 0.0, false);
            }
            if t == 300 {
                assert_eq!(p.air, 0);
            }
            if !p.alive() {
                dead_at = t;
                break;
            }
        }
        assert_eq!(first, 320);
        assert_eq!(dead_at, 500);
    }

    #[test]
    fn air_regain_75_ticks() {
        let mut p = h();
        p.air = 0;
        let mut n = 0;
        while p.air < MAX_AIR {
            p.tick_air(false);
            n += 1;
        }
        assert_eq!(n, 75);
    }

    #[test]
    fn bubbles_asp12() {
        assert_eq!(air_bubbles(300), (10, 0));
        assert_eq!(air_bubbles(273), (10, 0));
        assert_eq!(air_bubbles(272), (9, 1));
        assert_eq!(air_bubbles(271), (9, 1));
        assert_eq!(air_bubbles(270), (9, 0));
        assert_eq!(air_bubbles(32), (1, 1));
        assert_eq!(air_bubbles(30), (1, 0));
        assert_eq!(air_bubbles(2), (0, 1));
        assert_eq!(air_bubbles(1), (0, 1));
        assert_eq!(air_bubbles(0), (0, 0));
    }

    /// Регенерация: удар в T → лечение на T+140, T+180…; от 10 HP до 20 — последний +1 на T+500 [вариант A].
    #[test]
    fn regen_variant_a() {
        let mut p = h();
        p.begin();
        p.damage(DamageKind::Generic, 10.0, 0.0, false).unwrap();
        let mut heals = Vec::new();
        for t in 1..=600 {
            p.begin();
            if p.regen() {
                heals.push(t);
            }
        }
        assert_eq!(heals[0], 140);
        assert_eq!(heals[1], 180);
        assert_eq!(*heals.last().unwrap(), 500);
        assert_eq!(heals.len(), 10);
        assert_eq!(p.hp, 20.0);
    }

    #[test]
    fn heart_layout() {
        let row = hearts_row(7, 10);
        assert_eq!(row[0], [2, 2]);
        assert_eq!(row[3], [1, 2]); // сердце 3: HP 7–8; здоровье 7 — половинка, призрак 10 — целое
        assert_eq!(row[2], [2, 2]);
        let r = hearts_row(7, 7);
        assert_eq!(r[3], [1, 1]); // сердце 3: HP 7–8 → половинка
        assert_eq!(r[4], [0, 0]);
        assert_eq!(hearts_row(20, 20)[9], [2, 2]);
        assert_eq!(hearts_row(0, 0)[0], [0, 0]);
    }

    #[test]
    fn tilt_curve() {
        let near = |a: f32, b: f32| assert!((a - b).abs() < 0.05, "{a} ≠ {b}");
        near(hurt_tilt_deg(9.0), -12.35);
        near(hurt_tilt_deg(8.41), -14.0);
        near(hurt_tilt_deg(7.0), -9.59);
        near(hurt_tilt_deg(5.0), -2.73);
        near(hurt_tilt_deg(10.0), 0.0);
        assert_eq!(hurt_tilt_deg(-0.1), 0.0);
        near(death_tilt_deg(20.0), 40.0 - 8000.0 / 220.0);
        near(70.0 / death_fov_div(20.0), 65.0);
    }

    #[test]
    fn eat_rules() {
        let mut p = h();
        assert!(!p.can_eat(), "полные сердца");
        p.hp = 12.0;
        assert!(p.can_eat());
        p.heal(4.0);
        assert_eq!(p.hp, 16.0);
        p.hp = 18.0;
        p.heal(4.0);
        assert_eq!(p.hp, 20.0, "зажим");
    }

    #[test]
    fn burn_and_extinguish() {
        let mut p = h();
        p.ignite(BURN_TICKS);
        let mut dmg = 0;
        for _ in 0..BURN_TICKS {
            if p.tick_fire(false, false).is_some() {
                dmg += 1;
            }
        }
        assert_eq!(dmg, 15, "1 урона в секунду 15 секунд");
        p.ignite(BURN_TICKS);
        assert!(p.tick_fire(true, false).is_none());
        assert_eq!(p.fire, 0, "вода тушит");
    }

    #[test]
    fn death_counter() {
        let mut p = h();
        p.hp = 0.0;
        let mut removed_at = 0;
        for t in 1..=25 {
            if p.base_tick_end() {
                removed_at = t;
            }
        }
        assert_eq!(removed_at, 20);
        assert!(p.damage(DamageKind::Fall, 3.0, 0.0, false).is_none(), "мёртвый урона не принимает");
    }
}
