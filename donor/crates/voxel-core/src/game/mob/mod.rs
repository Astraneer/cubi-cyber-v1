//! Мобы (G6; mc/aspects ASP-16 общее, ASP-17 мирные, ASP-18 зомби): свинья, корова, овца, курица, зомби. Всё считает
//! сервер (Sim.mobs, фаза «мир → сущности»), клиент — интерполяция по тикам сервера и позы (view.rs), JS рисует кубоиды
//! (web/mobs.js).
//!
//! Тик моба (ASP-16 §2.2): prev = pos → вода и огонь (baseTick) → окно урона, смерть (20 тиков на боку, потом удаление)
//! → голос → ИИ: цели (ai.rs, полный пересчёт раз в 2 тика), путь (path.rs), MoveControl, LookControl, JumpControl →
//! прыжок или всплытие → ход (body.rs: та же коллизия, ступенька 0.6, гравитация 0.08, трение 0.546, что у игрока) →
//! поворот тела (BodyControl). После всех — расталкивание. Тикают только мобы в активной зоне (9×9 кусков игроков);
//! животные вне зоны «спят» и видны замершими, монстры вне зоны исчезают (spawn.rs).
//!
//! Скорость моба квадратична по атрибуту: MoveControl ставит movementSpeed = mult·attr и тот же forward, ускорение на
//! земле 0.98·ms² (ASP-16 §3.2). Урон — общий `Health` игрока (окно 20/10, броня), отбрасывание 0.4 «физическое»:
//! половина своей скорости сохраняется (ASP-12 §5).

pub(crate) mod ai;
pub(crate) mod body;
pub(crate) mod path;
pub(crate) mod spawn;
pub(crate) mod view;
#[cfg(test)]
mod tests;

use std::collections::{HashSet, VecDeque};

use crate::abi::{Ev, Sfx};
use crate::game::health::{DamageKind, Health};
use crate::game::physics::Bufs;
use crate::game::sim::Sim;
use crate::game::view::Events;
use crate::game::GameRng;
use crate::proto::{MobMove, MM_ARMS, MM_BABY, MM_DEAD, MM_EAT, MM_FIRE, MM_GROUND, MM_SHEARED, MM_WATER};
use crate::registry::ids::*;
use crate::registry::items::*;
use crate::registry::shapes::Aabb;
use crate::world::stack::ItemStack;
use crate::world::ticks::Zone;
use crate::world::WorldData;
use ai::Ai;

/// Вид моба: номер в протоколе (Ent::Mob.kind) — только дописывается.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub(crate) enum MobKind {
    Pig = 0,
    Cow = 1,
    Sheep = 2,
    Chicken = 3,
    Zombie = 4,
}

impl MobKind {
    pub(crate) const ALL: [MobKind; 5] = [MobKind::Pig, MobKind::Cow, MobKind::Sheep, MobKind::Chicken, MobKind::Zombie];
    pub(crate) fn from_u8(k: u8) -> Option<MobKind> {
        MobKind::ALL.get(k as usize).copied()
    }
    pub(crate) fn def(self) -> &'static KindDef {
        &KINDS[self as usize]
    }
    /// Вид по имени Minecraft (сцены, отладка): «pig», «cow», «sheep», «chicken», «zombie».
    pub(crate) fn by_name(s: &str) -> Option<MobKind> {
        MobKind::ALL.iter().copied().find(|k| k.def().mc == s)
    }
}

/// Таблица вида (ASP-16 §3.11, ASP-17 §2.1, ASP-18 §3).
pub(crate) struct KindDef {
    pub(crate) mc: &'static str,
    /// Ширина и высота коробки, высота глаз (блоки) взрослого.
    pub(crate) w: f64,
    pub(crate) h: f64,
    pub(crate) eye: f64,
    pub(crate) hp: f32,
    /// movement_speed; множитель паники (цель EscapeDanger).
    pub(crate) speed: f32,
    pub(crate) panic: f32,
    pub(crate) armor: f32,
    pub(crate) attack: f32,
    /// follow_range: дальность поиска цели и длина пути; лимит раскрытий A* — floor(follow·16).
    pub(crate) follow: f64,
    pub(crate) hostile: bool,
    pub(crate) fall_immune: bool,
    /// Задержка голоса (getMinAmbientSoundDelay): животные 120, монстры 80.
    pub(crate) ambient_delay: i32,
    /// Радиус «смотреть на игрока».
    pub(crate) look_range: f64,
    /// Штраф пути через воду и берег (курица лезет в воду — 0).
    pub(crate) water_penalty: f32,
    /// Голос, урон, смерть, шаг.
    pub(crate) snd: [Sfx; 4],
    /// Громкость звуков (корова 0.4).
    pub(crate) vol: f32,
    /// Радиус тени (для JS).
    pub(crate) shadow: f32,
}

pub(crate) static KINDS: [KindDef; 5] = [
    KindDef {
        mc: "pig",
        w: 0.9,
        h: 0.9,
        eye: 0.765,
        hp: 10.0,
        speed: 0.25,
        panic: 1.25,
        armor: 0.0,
        attack: 0.0,
        follow: 16.0,
        hostile: false,
        fall_immune: false,
        ambient_delay: 120,
        look_range: 6.0,
        water_penalty: 8.0,
        snd: [Sfx::PigAmbient, Sfx::PigHurt, Sfx::PigDeath, Sfx::PigStep],
        vol: 1.0,
        shadow: 0.7,
    },
    KindDef {
        mc: "cow",
        w: 0.9,
        h: 1.4,
        eye: 1.3,
        hp: 10.0,
        speed: 0.2,
        panic: 2.0,
        armor: 0.0,
        attack: 0.0,
        follow: 16.0,
        hostile: false,
        fall_immune: false,
        ambient_delay: 120,
        look_range: 6.0,
        water_penalty: 8.0,
        snd: [Sfx::CowAmbient, Sfx::CowHurt, Sfx::CowDeath, Sfx::CowStep],
        vol: 0.4,
        shadow: 0.7,
    },
    KindDef {
        mc: "sheep",
        w: 0.9,
        h: 1.3,
        eye: 1.235,
        hp: 8.0,
        speed: 0.23,
        panic: 1.25,
        armor: 0.0,
        attack: 0.0,
        follow: 16.0,
        hostile: false,
        fall_immune: false,
        ambient_delay: 120,
        look_range: 6.0,
        water_penalty: 8.0,
        snd: [Sfx::SheepAmbient, Sfx::SheepHurt, Sfx::SheepDeath, Sfx::SheepStep],
        vol: 1.0,
        shadow: 0.7,
    },
    KindDef {
        mc: "chicken",
        w: 0.4,
        h: 0.7,
        eye: 0.644,
        hp: 4.0,
        speed: 0.25,
        panic: 1.4,
        armor: 0.0,
        attack: 0.0,
        follow: 16.0,
        hostile: false,
        fall_immune: true,
        ambient_delay: 120,
        look_range: 6.0,
        water_penalty: 0.0,
        snd: [Sfx::ChickenAmbient, Sfx::ChickenHurt, Sfx::ChickenDeath, Sfx::ChickenStep],
        vol: 1.0,
        shadow: 0.3,
    },
    KindDef {
        mc: "zombie",
        w: 0.6,
        h: 1.95,
        eye: 1.74,
        hp: 20.0,
        speed: 0.23,
        panic: 1.0,
        armor: 2.0,
        attack: 3.0,
        follow: 35.0,
        hostile: true,
        fall_immune: false,
        ambient_delay: 80,
        look_range: 8.0,
        water_penalty: 8.0,
        snd: [Sfx::ZombieAmbient, Sfx::ZombieHurt, Sfx::ZombieDeath, Sfx::ZombieStep],
        vol: 1.0,
        shadow: 0.5,
    },
];

/// Цвета овцы (DyeColor природные, ASP-17 §5.1): номер — var в MobMove и смещение блока шерсти от WHITE_WOOL.
pub(crate) const SHEEP_WHITE: u8 = 0;
pub(crate) const SHEEP_BLACK: u8 = 1;
pub(crate) const SHEEP_GRAY: u8 = 2;
pub(crate) const SHEEP_LIGHT_GRAY: u8 = 3;
pub(crate) const SHEEP_BROWN: u8 = 4;
pub(crate) const SHEEP_PINK: u8 = 5;

/// Тиков на боку после смерти (LivingEntity.updatePostDeath).
pub(crate) const DEATH_TICKS: i32 = 20;
/// Предел мобов в мире (ASP-16 §12: отладочный спаун выше — отказ).
pub(crate) const MOB_LIMIT: usize = 400;

/// Игрок, каким его видит ИИ (снимок на тик мира): номер, ступни, глаза, коробка, жив, «творческий» (полёт отладки —
/// не цель), присед (дальность ×0.8), предметы в руках (соблазн).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Seen {
    pub(crate) id: u32,
    pub(crate) pos: [f64; 3],
    pub(crate) eye: [f64; 3],
    pub(crate) bbox: Aabb,
    pub(crate) alive: bool,
    pub(crate) creative: bool,
    pub(crate) crouch: bool,
    pub(crate) hands: [ItemId; 2],
}

/// Что мобы сделали с миром и игроками за тик — применяет сервер (урон через survival, блоки через set_world).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Out {
    /// Удар по игроку: кто бил (номер моба), игрок, урон, точка бьющего (направление отбрасывания).
    Hit { mob: u32, player: u32, amount: f32, from: [f64; 3] },
    /// Овца съела траву: растение в клетке (→ воздух) или блок травы (→ земля).
    EatGrass { pos: [i32; 3] },
}

/// Моб на сервере.
pub(crate) struct Mob {
    pub(crate) id: u32,
    pub(crate) kind: MobKind,
    pub(crate) baby: bool,
    /// Тиков с появления (age) — чётность полного тика ИИ.
    pub(crate) age: u32,
    pub(crate) pos: [f64; 3],
    pub(crate) prev: [f64; 3],
    pub(crate) vel: [f64; 3],
    /// Направление хода (yaw, MoveControl), тело (bodyYaw), голова (headYaw), тангаж головы — градусы Minecraft.
    pub(crate) yaw: f32,
    pub(crate) body: f32,
    pub(crate) head: f32,
    pub(crate) pitch: f32,
    pub(crate) on_ground: bool,
    pub(crate) h_coll: bool,
    pub(crate) fall: f32,
    pub(crate) in_water: bool,
    pub(crate) fluid_h: f64,
    pub(crate) in_lava: bool,
    pub(crate) eye_water: bool,
    pub(crate) jump_cd: i32,
    /// Запрос прыжка этого тика (JumpControl).
    pub(crate) jumping: bool,
    /// Ввод вперёд (forwardSpeed) и скорость (movementSpeed) — их ставит MoveControl.
    pub(crate) forward: f32,
    pub(crate) speed: f32,
    pub(crate) life: Health,
    /// Последний урон: тик мира и «паникующий» ли тип (panic_causes: удар, кактус, огонь; не падение).
    pub(crate) hurt_at: Option<u64>,
    pub(crate) hurt_panics: bool,
    /// Последний ударивший игрок и когда (RevengeGoal); «убит игроком» — удар игрока не старше 100 тиков.
    pub(crate) attacker: Option<(u32, u64)>,
    pub(crate) ambient: i32,
    /// despawnCounter: растёт, пока игрок дальше 32 блоков (≥ 100 — животное не начинает блуждать).
    pub(crate) despawn: i32,
    pub(crate) ai: Ai,
    pub(crate) sheared: bool,
    pub(crate) color: u8,
    /// Таймер яйца курицы.
    pub(crate) egg: i32,
    /// Овца щиплет траву: тиков до конца (40 → 0).
    pub(crate) eating: i32,
    /// Цель атаки (игрок) и руки зомби.
    pub(crate) target: Option<u32>,
    pub(crate) arms: bool,
    pub(crate) removed: bool,
    pub(crate) rng: GameRng,
}

impl Mob {
    pub(crate) fn new(id: u32, kind: MobKind, pos: [f64; 3], yaw: f32, seed: u64) -> Mob {
        let d = kind.def();
        let mut life = Health::new();
        life.hp = d.hp;
        life.max = d.hp;
        life.spawn_protect = 0;
        let mut rng = GameRng(seed ^ (id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x30B5);
        let egg = 6000 + rng.below(6000);
        Mob {
            id,
            kind,
            baby: false,
            age: 0,
            pos,
            prev: pos,
            vel: [0.0, -0.0784, 0.0],
            yaw,
            body: yaw,
            head: yaw,
            pitch: 0.0,
            on_ground: true,
            h_coll: false,
            fall: 0.0,
            in_water: false,
            fluid_h: 0.0,
            in_lava: false,
            eye_water: false,
            jump_cd: 0,
            jumping: false,
            forward: 0.0,
            speed: 0.0,
            life,
            hurt_at: None,
            hurt_panics: false,
            attacker: None,
            ambient: 0,
            despawn: 0,
            ai: Ai::new(kind),
            sheared: false,
            color: SHEEP_WHITE,
            egg,
            eating: 0,
            target: None,
            arms: false,
            removed: false,
            rng,
        }
    }

    pub(crate) fn def(&self) -> &'static KindDef {
        self.kind.def()
    }
    /// Ширина и высота коробки (детёныш — вдвое меньше).
    pub(crate) fn dims(&self) -> (f64, f64) {
        let d = self.def();
        if self.baby {
            (d.w * 0.5, d.h * 0.5)
        } else {
            (d.w, d.h)
        }
    }
    pub(crate) fn bbox(&self) -> Aabb {
        let (w, h) = self.dims();
        let r = w / 2.0;
        let [x, y, z] = self.pos;
        Aabb { lo: [x - r, y, z - r], hi: [x + r, y + h, z + r] }
    }
    /// Высота глаз над ступнями (детёныш курицы и коровы — свои, прочие — половина).
    pub(crate) fn eye_h(&self) -> f64 {
        let d = self.def();
        match (self.baby, self.kind) {
            (false, _) => d.eye,
            (true, MobKind::Cow) => 0.665,
            (true, MobKind::Chicken) => 0.2975,
            (true, MobKind::Zombie) => 0.93,
            (true, _) => d.eye * 0.5,
        }
    }
    pub(crate) fn eye(&self) -> [f64; 3] {
        [self.pos[0], self.pos[1] + self.eye_h(), self.pos[2]]
    }
    pub(crate) fn alive(&self) -> bool {
        self.life.alive()
    }
    /// Атрибут скорости (детёныш-зомби +50 %).
    pub(crate) fn speed_attr(&self) -> f32 {
        let s = self.def().speed;
        if self.baby && self.kind == MobKind::Zombie {
            s * 1.5
        } else {
            s
        }
    }
    pub(crate) fn feet_cell(&self) -> [i32; 3] {
        [self.pos[0].floor() as i32, self.pos[1].floor() as i32, self.pos[2].floor() as i32]
    }
    /// Высота тона голоса (LivingEntity.getSoundPitch): (r − r)·0.2 + 1, детёныш + 0.5.
    fn voice_pitch(&self, fx: &mut GameRng) -> f32 {
        (fx.next_f32() - fx.next_f32()) * 0.2 + if self.baby { 1.5 } else { 1.0 }
    }
    /// Звук моба у ступней: громкость вида и высота тона в параметре (abi: биты 10–19 громкость ×1000, 20–30 высота ×1000).
    pub(crate) fn sound(&self, ev: &mut Events, sfx: Sfx, pitch: f32) {
        let vol = (self.def().vol * 1000.0).round() as i32;
        let p = ((pitch * 1000.0).round() as i32).clamp(1, 2047);
        ev.push(Ev::Sound as i32, self.feet_cell(), sfx as i32 | (vol.clamp(0, 1023) << 10) | (p << 20));
    }
    /// Голос, урон, смерть со «своей» высотой тона.
    pub(crate) fn voice(&self, ev: &mut Events, fx: &mut GameRng, k: usize) {
        let p = self.voice_pitch(fx);
        self.sound(ev, self.def().snd[k], p);
    }

    /// Состояние для клиентов.
    pub(crate) fn moved(&self) -> MobMove {
        let mut flags = 0;
        for (on, f) in [
            (self.on_ground, MM_GROUND),
            (!self.alive(), MM_DEAD),
            (self.baby, MM_BABY),
            (self.sheared, MM_SHEARED),
            (self.arms, MM_ARMS),
            (self.eating > 0, MM_EAT),
            (self.life.fire > 0, MM_FIRE),
            (self.in_water, MM_WATER),
        ] {
            if on {
                flags |= f;
            }
        }
        MobMove { id: self.id, pos: self.pos.map(|v| v as f32), body: self.body, head: self.head, pitch: self.pitch, flags, var: self.color }
    }

    /// Добыча при смерти (loot_table/entities/*, ASP-17 §6.1, ASP-18 §10): стеки в точке ступней. Детёныш животного не
    /// роняет ничего; горевший — жареное мясо (furnace_smelt). by_player — удар игрока за последние 100 тиков.
    pub(crate) fn loot(&mut self, rng: &mut GameRng, by_player: bool) -> Vec<ItemStack> {
        let mut out = Vec::new();
        if self.baby && self.kind != MobKind::Zombie {
            return out;
        }
        let fire = self.life.fire > 0;
        let meat = |raw: ItemId, cooked: ItemId| if fire { cooked } else { raw };
        let mut add = |item: ItemId, n: i32| {
            if n > 0 && item != 0 {
                out.push(ItemStack::of(item, n));
            }
        };
        match self.kind {
            MobKind::Pig => add(meat(I_PORKCHOP, I_COOKED_PORKCHOP), rng.range(1, 3)),
            MobKind::Cow => {
                add(I_LEATHER, rng.range(0, 2));
                add(meat(I_BEEF, I_COOKED_BEEF), rng.range(1, 3));
            }
            MobKind::Sheep => {
                if !self.sheared {
                    add(WHITE_WOOL as ItemId + self.color as ItemId, 1);
                }
                add(meat(I_MUTTON, I_COOKED_MUTTON), rng.range(1, 2));
            }
            MobKind::Chicken => {
                add(I_FEATHER, rng.range(0, 2));
                add(meat(I_CHICKEN, I_COOKED_CHICKEN), 1);
            }
            MobKind::Zombie => {
                add(I_ROTTEN_FLESH, rng.range(0, 2));
                // Редкое (killed_by_player): 2.5 % — железный слиток, морковь или картофель (печёный, если горел). Морковь и
                // картофель — по имени: их заводит команда фермы (нет в реестре — ничего).
                if by_player && rng.next_f32() < 0.025 {
                    let pick = rng.below(3);
                    let item = match pick {
                        0 => I_IRON_INGOT,
                        1 => item_by_mc("carrot"),
                        _ => item_by_mc(if fire { "baked_potato" } else { "potato" }),
                    };
                    add(item, 1);
                }
            }
        }
        out
    }
}

/// Мобы мира сервера: список в порядке появления (тик по снимку), заселённые стадами куски (раз за сессию), ГСЧ
/// спауна и звуков, бюджет поиска пути на тик.
pub(crate) struct Mobs {
    pub(crate) list: Vec<Mob>,
    /// Куски, где уже были стада (ASP-16 §6.8 п. 2): раз за сессию; очередь — по близости к игрокам.
    pub(crate) populated: HashSet<(i32, i32)>,
    pub(crate) pop_queue: VecDeque<(i32, i32)>,
    pub(crate) spawn_rng: GameRng,
    pub(crate) fx: GameRng,
    pub(crate) bufs: Bufs,
    /// Естественный спаун зомби (ночь и темнота) и стада при генерации; выключаются проверками.
    pub(crate) natural: bool,
    pub(crate) herds: bool,
    /// Предел монстров на 289 кусков (ваниль 70; масштаб по площади зоны — ASP-16 §6.8 п. 5).
    pub(crate) monster_cap: f64,
    /// Анимации этого тика для клиентов: (номер, вид: 1 — урон).
    pub(crate) anims: Vec<(u32, u8)>,
    pub(crate) seed: u64,
    /// Счётчики: раскрытий A* за тик (сумма с начала), ИИ-тиков.
    pub(crate) stats: MobStats,
}

#[derive(Default, Clone, Debug)]
pub(crate) struct MobStats {
    pub(crate) path_nodes: u64,
    pub(crate) paths: u64,
    pub(crate) spawned: u64,
    pub(crate) despawned: u64,
}

/// Раскрытий A* на все поиски за тик (ASP-16 §12): остальные запросы ждут следующего тика.
pub(crate) const PATH_BUDGET: i32 = 2000;

impl Mobs {
    pub(crate) fn new(seed: u64) -> Mobs {
        Mobs {
            list: Vec::new(),
            populated: HashSet::new(),
            pop_queue: VecDeque::new(),
            spawn_rng: GameRng(seed ^ 0x5A57_17E0),
            fx: GameRng(seed ^ 0xF00D_5EED),
            bufs: Bufs::default(),
            natural: true,
            herds: true,
            monster_cap: 70.0,
            anims: Vec::new(),
            seed,
            stats: MobStats::default(),
        }
    }
    pub(crate) fn get(&self, id: u32) -> Option<&Mob> {
        self.list.iter().find(|m| m.id == id && !m.removed)
    }
    pub(crate) fn index(&self, id: u32) -> Option<usize> {
        self.list.iter().position(|m| m.id == id && !m.removed)
    }
    pub(crate) fn count(&self, hostile: bool) -> usize {
        self.list.iter().filter(|m| !m.removed && m.def().hostile == hostile).count()
    }
}

/// Источник урона моба: удар игрока (номер, точка, yaw и добавка отбрасывания за бег) или прочее.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Blow {
    pub(crate) kind: DamageKind,
    pub(crate) amount: f32,
    /// Ударивший игрок: номер, точка (направление отбрасывания), yaw, сила добавочного отбрасывания (бег — 1).
    pub(crate) by: Option<(u32, [f64; 3], f32, f32)>,
}

impl Sim {
    /// Новый моб в точке (ступни) со случайным yaw; ответ — номер. Овца — случайный цвет, детёныш — по флагу.
    pub(crate) fn spawn_mob(&mut self, kind: MobKind, pos: [f64; 3], baby: bool) -> Option<u32> {
        if self.mobs.list.len() >= MOB_LIMIT {
            return None;
        }
        let id = self.ids.next();
        let yaw = self.mobs.spawn_rng.next_f32() * 360.0;
        let mut m = Mob::new(id, kind, pos, yaw, self.mobs.seed);
        m.baby = baby;
        if kind == MobKind::Sheep {
            m.color = spawn::sheep_color(&mut self.mobs.spawn_rng);
        }
        self.mobs.list.push(m);
        self.mobs.stats.spawned += 1;
        Some(id)
    }

    /// Урон мобу (LivingEntity.damage): окно 20/10, броня, отбрасывание от игрока, паника, месть; смерть — добыча сразу,
    /// звук смерти. true — урон прошёл.
    pub(crate) fn hurt_mob(&mut self, i: usize, b: Blow, ev: &mut Events) -> bool {
        let time = self.time;
        let fx = &mut self.mobs.fx;
        let m = &mut self.mobs.list[i];
        if m.removed || !m.alive() {
            return false;
        }
        let armor = m.def().armor;
        let Some(hit) = m.life.damage(b.kind, b.amount, armor, false) else { return false };
        m.hurt_at = Some(time);
        m.hurt_panics = matches!(b.kind, DamageKind::PlayerAttack | DamageKind::MobAttack | DamageKind::Cactus | DamageKind::OnFire | DamageKind::InFire | DamageKind::Lava);
        if let Some((pid, from, yaw, extra)) = b.by {
            m.attacker = Some((pid, time));
            if hit.full {
                // takeKnockback(0.4, x_игрока − x_моба, z_игрока − z_моба); бег — ещё (0.5·1, sin yaw, −cos yaw).
                knockback(m, 0.4, from[0] - m.pos[0], from[2] - m.pos[2]);
                if extra > 0.0 {
                    let r = (yaw as f64).to_radians();
                    knockback(m, extra as f64 * 0.5, libm::sin(r), -libm::cos(r));
                }
            }
        }
        if hit.full {
            self.mobs.anims.push((m.id, 1));
        }
        if hit.died {
            m.voice(ev, fx, 2);
            m.ai.nav.stop();
            let by_player = m.attacker.is_some_and(|(_, t)| time.saturating_sub(t) <= 100);
            let at = m.pos;
            let drops = m.loot(&mut self.rng, by_player);
            for s in drops {
                self.items.drop_at(at, s, &mut self.rng, &mut self.ids);
            }
        } else if hit.full {
            m.voice(ev, fx, 1);
            m.ambient = -m.def().ambient_delay;
        }
        true
    }
}

/// LivingEntity.takeKnockback(strength, x, z): v.xz/2 − норм(x, z)·strength; на земле vy = min(0.4, vy/2 + strength).
pub(crate) fn knockback(m: &mut Mob, strength: f64, x: f64, z: f64) {
    if strength <= 0.0 {
        return;
    }
    let mut d = (x * x + z * z).sqrt();
    let (mut dx, mut dz) = (x, z);
    if d < 1e-4 {
        dx = 0.0;
        dz = 0.0;
        d = 1.0;
    }
    let (nx, nz) = (dx / d * strength, dz / d * strength);
    let v = m.vel;
    m.vel = [v[0] / 2.0 - nx, if m.on_ground { (v[1] / 2.0 + strength).min(0.4) } else { v[1] }, v[2] / 2.0 - nz];
}

/// Зона, где мобы тикают, и снимок игроков — для тика мобов (Sim::tick_mobs в spawn.rs и ai.rs).
pub(crate) struct TickCtx<'a> {
    pub(crate) players: &'a [Seen],
    pub(crate) zone: &'a Zone,
}

/// Блок под ногами «трава» (тег animals_spawnable_on — блок травы любого варианта).
pub(crate) fn is_grass_block(w: &mut WorldData, x: i32, y: i32, z: i32) -> bool {
    crate::registry::blocks::bdef(w.get_block(x, y, z)).grass
}
