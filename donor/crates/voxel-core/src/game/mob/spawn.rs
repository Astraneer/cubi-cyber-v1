//! Тик мобов мира, спаун и деспаун (ASP-16 §2, §6; ASP-17 §6.3; ASP-18 §6, §11).
//!
//! - Стада при «генерации куска»: куски в R_VIS кусков от игроков заселяются раз за сессию (ГСЧ — от сида и куска:
//!   стада те же при каждом запуске), не больше POP_PER_TICK кусков за тик, ближние первыми.
//! - Зомби: каждый тик в каждом куске активной зоны — попытка (3 стаи до 4), пока монстров меньше капа
//!   70 × куски_зоны / 289; вид — по весам биома, «пустые» веса отсутствующих монстров остаются (зомби 95 из 515); свет
//!   — небо по столбу (до движка света ASP-14), ночная тьма — по времени суток; не ближе 24 к игроку, не на Перекрёстке.
//! - Деспаун: монстр вне зоны — сразу; дальше 32 от игроков и счётчик > 600 — шанс 1/800 за тик; животные не исчезают.
//! - Зомби горят днём под открытым небом (4 %/тик на полном солнце, поджог на 160 тиков; шлема у зомби нет).

use super::ai::{self, sky_light, AiCtx};
use super::{Blow, MobKind, Out, Seen, DEATH_TICKS, SHEEP_BLACK, SHEEP_BROWN, SHEEP_GRAY, SHEEP_LIGHT_GRAY, SHEEP_PINK, SHEEP_WHITE};
use crate::abi::{Ev, Sfx};
use crate::cell::{block_of, CS, H};
use crate::game::health::{fall_damage, DamageKind, BURN_TICKS};
use crate::game::physics::space_empty;
use crate::game::sim::Sim;
use crate::game::view::Events;
use crate::game::GameRng;
use crate::gen::Biome;
use crate::registry::blocks::{bdef, collision_of, face_full};
use crate::registry::ids::*;
use crate::registry::items::I_EGG;
use crate::world::stack::ItemStack;
use crate::world::ticks::Zone;
use crate::world::WorldData;

/// Радиус заселения стадами (куски): ~128 блоков (R_vis, ASP-16 §6.8 п. 2).
pub(crate) const R_VIS: i32 = 8;
/// Кусков, заселяемых за тик (каждый — поиск высоты и проверки места).
const POP_PER_TICK: usize = 6;
/// Ваниль: у монстров зомби 95 из 515 по весу (§6.6) — остальные веса пусты.
const ZOMBIE_WEIGHT: i32 = 95;
const MONSTER_WEIGHT: i32 = 515;

/// Цвет новой овцы (SheepEntity.generateDefaultColor).
pub(crate) fn sheep_color(r: &mut GameRng) -> u8 {
    let i = r.below(100);
    match i {
        0..=4 => SHEEP_BLACK,
        5..=9 => SHEEP_GRAY,
        10..=14 => SHEEP_LIGHT_GRAY,
        15..=17 => SHEEP_BROWN,
        _ => {
            if r.below(500) == 0 {
                SHEEP_PINK
            } else {
                SHEEP_WHITE
            }
        }
    }
}

/// Тьма неба (World.calculateAmbientDarkness без дождя) по времени суток day (0 — 6:00): полдень 0, полночь 11.
pub(crate) fn ambient_darkness(day: u64) -> i32 {
    let d = ((day as f64) / 24000.0 - 0.25).rem_euclid(1.0);
    let e = 0.5 - libm::cos(d * std::f64::consts::PI) / 2.0;
    let angle = ((d * 2.0 + e) / 3.0) as f32;
    let c = libm::cosf(angle * std::f32::consts::TAU) as f64;
    let f = 0.5 + 2.0 * c.clamp(-0.25, 0.25);
    ((1.0 - f) * 11.0) as i32
}

/// Стадо: веса видов биома (овца, свинья, курица, корова) и «пустой» вес чужих видов; вероятность стада на кусок.
fn herd_table(b: Biome) -> Option<([i32; 4], i32, f32)> {
    let ours = [12, 10, 10, 8];
    match b {
        Biome::Plains => Some((ours, 6, 0.1)),
        Biome::Forest | Biome::Mountain => Some((ours, 5, 0.1)),
        Biome::Birch => Some((ours, 0, 0.1)),
        Biome::Taiga | Biome::Snowy => Some((ours, 20, 0.1)),
        Biome::Savanna => Some((ours, 12, 0.1)),
        _ => None,
    }
}

/// Место «на земле» для коробки моба в клетке p: опора сверху полная (не листва, не жидкость), клетки тела не полные
/// кубы и без жидкости, коробка пуста.
fn ground_ok(w: &mut WorldData, p: [i32; 3], kind: MobKind, bufs: &mut Vec<crate::registry::shapes::Aabb>) -> bool {
    if p[1] < 1 || p[1] + 2 >= H {
        return false;
    }
    let below = w.get_cell(p[0], p[1] - 1, p[2]);
    let bd = bdef(block_of(below));
    if bd.leaves || bd.fluid != 0 || !face_full(below, crate::cell::F_UP) {
        return false;
    }
    for k in 0..2 {
        let c = w.get_cell(p[0], p[1] + k, p[2]);
        let d = bdef(block_of(c));
        if d.fluid != 0 || (d.suffocates && !collision_of(c).is_empty()) {
            return false;
        }
    }
    let d = kind.def();
    let r = d.w / 2.0;
    let (x, y, z) = (p[0] as f64 + 0.5, p[1] as f64, p[2] as f64 + 0.5);
    let b = crate::registry::shapes::Aabb { lo: [x - r, y, z - r], hi: [x + r, y + d.h, z + r] };
    space_empty(w, &b, bufs)
}

impl Sim {
    /// Фаза «мир → мобы» (после предметов): стада, спаун зомби, деспаун, тик мобов зоны, расталкивание, уборка тел.
    /// Ответ — удары по игрокам (сервер проводит их через survival) .
    pub(crate) fn tick_mobs(&mut self, players: &[Seen], ev: &mut Events) -> Vec<Out> {
        let zone = std::mem::take(&mut self.zone);
        let d = self.world.d.clone();
        let mut w = d.borrow_mut();
        if self.mobs.herds {
            self.populate(&mut w, players);
        }
        let dark = ambient_darkness(self.day);
        if self.mobs.natural && !players.is_empty() {
            self.spawn_monsters(&mut w, players, &zone, dark);
        }
        self.despawn(players, &zone);
        let mut out = Vec::new();
        let mut revenge = Vec::new();
        let mut budget = super::PATH_BUDGET;
        let n = self.mobs.list.len();
        for i in 0..n {
            if self.mobs.list[i].removed {
                continue;
            }
            if !zone.has_pos(self.mobs.list[i].pos) {
                // Спит: не тикает, рисуется замершим.
                let m = &mut self.mobs.list[i];
                m.prev = m.pos;
                continue;
            }
            self.tick_mob(i, &mut w, players, dark, &mut budget, &mut out, &mut revenge, ev);
        }
        // Месть стаей (RevengeGoal.setGroupRevenge): зомби без цели в коробке ±35 × ±10 × ±35 — на того же игрока.
        for (who, pid) in revenge {
            let Some(at) = self.mobs.get(who).map(|m| m.pos) else { continue };
            for m in self.mobs.list.iter_mut() {
                if m.id != who && !m.removed && m.alive() && m.kind == MobKind::Zombie && m.target.is_none() {
                    if (m.pos[0] - at[0]).abs() <= 35.0 && (m.pos[1] - at[1]).abs() <= 10.0 && (m.pos[2] - at[2]).abs() <= 35.0 {
                        m.target = Some(pid);
                        m.attacker = Some((pid, self.time));
                    }
                }
            }
        }
        self.push_apart(players, &zone);
        // Овца съела траву: растение → воздух, блок травы → земля (событие ломания: звук и крошки).
        for o in &out {
            if let Out::EatGrass { pos } = *o {
                let b = w.get_block(pos[0], pos[1], pos[2]);
                if b == TALL_GRASS {
                    ev.push(Ev::Broken as i32, pos, b as i32);
                    self.set_world(&mut w, pos, AIR as u32, ev);
                } else if bdef(b).grass {
                    ev.push(Ev::Broken as i32, pos, b as i32);
                    self.set_world(&mut w, pos, DIRT as u32, ev);
                }
            }
        }
        self.mobs.list.retain(|m| !m.removed);
        self.zone = zone;
        out
    }

    /// Один тик моба i (ASP-16 §2.2).
    #[allow(clippy::too_many_arguments)]
    fn tick_mob(&mut self, i: usize, w: &mut WorldData, players: &[Seen], dark: i32, budget: &mut i32, out: &mut Vec<Out>, revenge: &mut Vec<(u32, u32)>, ev: &mut Events) {
        let time = self.time;
        let mut blows: Vec<Blow> = Vec::new();
        {
            let fx = &mut self.mobs.fx;
            let m = &mut self.mobs.list[i];
            m.prev = m.pos;
            m.age += 1;
            if m.pos[1] < -64.0 {
                m.removed = true;
                return;
            }
            // baseTick: вода и лава, огонь, воздух; окно урона −1; смерть — 20 тиков на боку.
            let was_fire = m.life.fire > 0;
            m.update_fluids(w);
            m.life.begin();
            if m.alive() {
                if let Some(dmg) = m.life.tick_fire(m.in_water, m.in_lava) {
                    blows.push(Blow { kind: DamageKind::OnFire, amount: dmg, by: None });
                }
                if was_fire && m.life.fire == 0 && m.in_water {
                    m.sound(ev, Sfx::Extinguish, 1.6 + (fx.next_f32() - fx.next_f32()) * 0.4);
                }
                if m.in_lava {
                    m.life.ignite(BURN_TICKS);
                    blows.push(Blow { kind: DamageKind::Lava, amount: 4.0, by: None });
                }
                // Зомби и другие нежить под водой не дышат по-нашему: тонут только животные.
                if m.kind != MobKind::Zombie {
                    if let Some(dmg) = m.life.tick_air(m.eye_water) {
                        blows.push(Blow { kind: DamageKind::Drown, amount: dmg, by: None });
                    }
                }
            }
            if m.life.base_tick_end() || m.life.death_time >= DEATH_TICKS {
                m.removed = true;
                return;
            }
        }
        for b in blows.drain(..) {
            self.hurt_mob(i, b, ev);
        }
        // Солнце (ZombieEntity.tickMovement): день, свет у глаз > 0.5, не мокрый, небо видно → шанс (яркость − 0.4)·2/30.
        if self.mobs.list[i].kind == MobKind::Zombie && self.mobs.list[i].alive() && dark < 4 {
            let m = &mut self.mobs.list[i];
            let e = m.eye();
            let c = [e[0].floor() as i32, e[1].floor() as i32, e[2].floor() as i32];
            let sky = sky_light(w, c[0], c[1], c[2]);
            let br = ai::brightness((sky - dark).max(0));
            if br > 0.5 && sky == 15 && !m.in_water && m.rng.next_f32() * 30.0 < (br - 0.4) * 2.0 {
                m.life.ignite(160);
            }
        }
        // Голос, яйцо, ИИ.
        let egg = {
            let fx = &mut self.mobs.fx;
            let m = &mut self.mobs.list[i];
            m.tick_ambient(ev, fx);
            m.tick_egg(ev, fx)
        };
        if egg {
            let at = self.mobs.list[i].pos;
            self.items.drop_at(at, ItemStack::of(I_EGG, 1), &mut self.rng, &mut self.ids);
        }
        let (landed, cactus) = {
            let Sim { mobs, .. } = self;
            let super::Mobs { list, fx, stats, bufs, .. } = mobs;
            let m = &mut list[i];
            if m.alive() {
                let mut c = AiCtx { w: &mut *w, players, time, dark, budget, out: &mut *out, revenge: &mut *revenge, stats, ev: &mut *ev, fx };
                m.tick_ai(&mut c);
            }
            let landed = m.tick_body(w, bufs);
            m.body_tick();
            let cactus = m.alive() && crate::game::hazards::cactus_touch(w, &m.bbox());
            (landed, cactus)
        };
        if let Some(fd) = landed {
            // Пашню топчут живые побольше курицы и детёнышей (ширина² · высота > 0.512, ASP-19 §2.7).
            let m = &self.mobs.list[i];
            let (mw, mh) = m.dims();
            let land = [m.pos[0].floor() as i32, (m.pos[1] - 0.2).floor() as i32, m.pos[2].floor() as i32];
            if w.get_block(land[0], land[1], land[2]) == crate::registry::ids::FARMLAND {
                self.land_on_farmland(w, land, fd, mw * mw * mh > 0.512, ev);
            }
            if !self.mobs.list[i].def().fall_immune {
                let dmg = fall_damage(fd);
                if dmg > 0 {
                    self.hurt_mob(i, Blow { kind: DamageKind::Fall, amount: dmg as f32, by: None }, ev);
                }
            }
        }
        if cactus {
            self.hurt_mob(i, Blow { kind: DamageKind::Cactus, amount: 1.0, by: None }, ev);
        }
    }

    /// Расталкивание (LivingEntity.tickCramming / Entity.pushAwayFrom): мобы в зоне друг друга — по разу из тика каждого
    /// (пара — дважды), игрок толкает моба дважды (из тика моба и игрока), сам игрок здесь не сдвигается.
    fn push_apart(&mut self, players: &[Seen], zone: &Zone) {
        let n = self.mobs.list.len();
        let boxes: Vec<_> = self.mobs.list.iter().map(|m| (m.bbox(), !m.removed && m.alive() && zone.has_pos(m.pos))).collect();
        let mut dv = vec![[0.0f64; 2]; n];
        for a in 0..n {
            if !boxes[a].1 {
                continue;
            }
            for b in a + 1..n {
                if !boxes[b].1 || !boxes[a].0.intersects(&boxes[b].0) {
                    continue;
                }
                if let Some((x, z)) = push_vec(self.mobs.list[a].pos, self.mobs.list[b].pos) {
                    dv[a][0] -= 2.0 * x;
                    dv[a][1] -= 2.0 * z;
                    dv[b][0] += 2.0 * x;
                    dv[b][1] += 2.0 * z;
                }
            }
            for p in players {
                if !p.alive || !boxes[a].0.intersects(&p.bbox) {
                    continue;
                }
                if let Some((x, z)) = push_vec(p.pos, self.mobs.list[a].pos) {
                    dv[a][0] += 2.0 * x;
                    dv[a][1] += 2.0 * z;
                }
            }
        }
        for (m, d) in self.mobs.list.iter_mut().zip(dv) {
            m.vel[0] += d[0];
            m.vel[2] += d[1];
        }
    }

    /// Деспаун (MobEntity.checkDespawn, ASP-16 §6.7–6.8): счётчик сбрасывается рядом с игроком (ближе 32).
    fn despawn(&mut self, players: &[Seen], zone: &Zone) {
        for m in self.mobs.list.iter_mut() {
            if m.removed {
                continue;
            }
            let d2 = players.iter().map(|p| (0..3).map(|k| (p.pos[k] - m.pos[k]).powi(2)).sum::<f64>()).fold(f64::MAX, f64::min);
            if !m.def().hostile {
                if d2 < 32.0 * 32.0 {
                    m.despawn = 0;
                }
                continue;
            }
            if !zone.has_pos(m.pos) || players.is_empty() {
                m.removed = true;
                self.mobs.stats.despawned += 1;
                continue;
            }
            if m.despawn > 600 && m.rng.below(800) == 0 && d2 > 32.0 * 32.0 {
                m.removed = true;
                self.mobs.stats.despawned += 1;
            } else if d2 < 32.0 * 32.0 {
                m.despawn = 0;
            }
        }
    }

    /// Стада (SpawnHelper.populateEntities) в кусках вокруг игроков, ещё не заселённых в этой сессии.
    fn populate(&mut self, w: &mut WorldData, players: &[Seen]) {
        for p in players {
            let (cx, cz) = ((p.pos[0].floor() as i32).div_euclid(CS), (p.pos[2].floor() as i32).div_euclid(CS));
            let mut fresh = Vec::new();
            for dz in -R_VIS..=R_VIS {
                for dx in -R_VIS..=R_VIS {
                    let c = (cx + dx, cz + dz);
                    if !self.mobs.populated.contains(&c) {
                        fresh.push((dx * dx + dz * dz, c));
                    }
                }
            }
            fresh.sort();
            for (_, c) in fresh {
                self.mobs.populated.insert(c);
                self.mobs.pop_queue.push_back(c);
            }
        }
        for _ in 0..POP_PER_TICK {
            let Some((cx, cz)) = self.mobs.pop_queue.pop_front() else { break };
            self.populate_chunk(w, cx, cz);
        }
    }

    /// Стада одного куска: пока rand < вероятности — вид по весу, стая из 4 (первый взрослый, дальше 5 % детёнышей),
    /// до 4 попыток на особь со сдвигом ±4 внутри куска; место — на поверхности, над блоком травы.
    pub(crate) fn populate_chunk(&mut self, w: &mut WorldData, cx: i32, cz: i32) {
        let seed = self.mobs.seed ^ (cx as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (cz as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        let mut r = GameRng(seed ^ 0x4E7D_5EED);
        let (x0, z0) = (cx * CS, cz * CS);
        let col = w.gen().column(x0 + 8, z0 + 8);
        let Some((weights, empty, prob)) = herd_table(col.biome) else { return };
        let kinds = [MobKind::Sheep, MobKind::Pig, MobKind::Chicken, MobKind::Cow];
        let mut bufs = Vec::new();
        let mut guard = 0;
        while r.next_f32() < prob && guard < 8 {
            guard += 1;
            let total: i32 = weights.iter().sum::<i32>() + empty;
            let mut pick = r.below(total);
            let mut kind = None;
            for (k, &wt) in weights.iter().enumerate() {
                if pick < wt {
                    kind = Some(kinds[k]);
                    break;
                }
                pick -= wt;
            }
            let Some(kind) = kind else { continue };
            let (ox, oz) = (x0 + r.below(CS), z0 + r.below(CS));
            let (mut x, mut z) = (ox, oz);
            for member in 0..4 {
                for _ in 0..4 {
                    let h = w.gen().column(x, z).h;
                    let mut y = (h + 1).min(H - 3);
                    // Правки: столб мог вырасти или стать ниже — поднять из земли, опустить в пустоту.
                    let mut k = 0;
                    while k < 8 && y + 1 < H && !collision_of(w.get_cell(x, y, z)).is_empty() {
                        y += 1;
                        k += 1;
                    }
                    while y > 1 && collision_of(w.get_cell(x, y - 1, z)).is_empty() && bdef(w.get_block(x, y - 1, z)).fluid == 0 && k < 16 {
                        y -= 1;
                        k += 1;
                    }
                    if super::is_grass_block(w, x, y - 1, z) && ground_ok(w, [x, y, z], kind, &mut bufs) {
                        let baby = member > 0 && r.next_f32() < 0.05;
                        let pos = [x as f64 + 0.5, y as f64, z as f64 + 0.5];
                        if let Some(id) = self.spawn_mob(kind, pos, baby) {
                            let yaw = r.next_f32() * 360.0;
                            if let Some(m) = self.mobs.list.iter_mut().rev().find(|m| m.id == id) {
                                (m.yaw, m.body, m.head) = (yaw, yaw, yaw);
                            }
                        }
                        break;
                    }
                    // Сдвиг ±4 внутри куска (от начала стаи).
                    let mut tries = 0;
                    loop {
                        x += r.below(5) - r.below(5);
                        z += r.below(5) - r.below(5);
                        if (x0..x0 + CS).contains(&x) && (z0..z0 + CS).contains(&z) || tries > 8 {
                            break;
                        }
                        x = ox;
                        z = oz;
                        tries += 1;
                    }
                    x = x.clamp(x0, x0 + CS - 1);
                    z = z.clamp(z0, z0 + CS - 1);
                }
            }
        }
    }

    /// Естественный спаун монстров (SpawnHelper.spawnEntitiesInChunk) в кусках зоны: кап 70 × куски/289.
    fn spawn_monsters(&mut self, w: &mut WorldData, players: &[Seen], zone: &Zone, dark: i32) {
        let cap = (self.mobs.monster_cap * zone.len() as f64 / 289.0).floor() as usize;
        let mut count = self.mobs.count(true);
        if count >= cap {
            return;
        }
        let mut bufs = Vec::new();
        let chunks: Vec<(i32, i32)> = zone.chunks().collect();
        let start = if chunks.is_empty() { 0 } else { self.mobs.spawn_rng.below(chunks.len() as i32) as usize };
        for k in 0..chunks.len() {
            if count >= cap {
                break;
            }
            let (cx, cz) = chunks[(start + k) % chunks.len()];
            count += self.spawn_in_chunk(w, players, cx, cz, dark, &mut bufs, cap - count);
        }
    }

    /// Одна попытка в куске: точка (x, z) в куске, y = U(−79, верх + 1) (поправка на наше море — ASP-16 §6.8 п. 6), три стаи
    /// до ceil(rand·4) шагов ±5. Ответ — сколько появилось (не больше 4 и left).
    #[allow(clippy::too_many_arguments)]
    fn spawn_in_chunk(&mut self, w: &mut WorldData, players: &[Seen], cx: i32, cz: i32, dark: i32, bufs: &mut Vec<crate::registry::shapes::Aabb>, left: usize) -> usize {
        let r = &mut self.mobs.spawn_rng;
        let (x, z) = (cx * CS + r.below(CS), cz * CS + r.below(CS));
        let top = w.gen().column(x, z).h + 1;
        let y = r.range(-79, top.max(1));
        if y < 1 || y >= H - 2 {
            return 0;
        }
        let c = w.get_cell(x, y, z);
        if bdef(block_of(c)).suffocates && !collision_of(c).is_empty() {
            return 0;
        }
        let mut made = 0;
        for _pack in 0..3 {
            let (mut px, mut pz) = (x, z);
            let mut chosen: Option<bool> = None;
            let steps = (self.mobs.spawn_rng.next_f32() * 4.0).ceil() as i32;
            for _ in 0..steps {
                let r = &mut self.mobs.spawn_rng;
                px += r.below(6) - r.below(6);
                pz += r.below(6) - r.below(6);
                let pos = [px as f64 + 0.5, y as f64, pz as f64 + 0.5];
                let near = players.iter().map(|p| (0..3).map(|k| (p.pos[k] - pos[k]).powi(2)).sum::<f64>()).fold(f64::MAX, f64::min);
                if near <= 576.0 || near > 128.0 * 128.0 {
                    continue;
                }
                if crate::game::interact::protected(w, [px, y, pz]) {
                    continue;
                }
                // Вид — один раз на стаю: зомби 95 из 515, остальное — пусто (стая пропала).
                let zombie = *chosen.get_or_insert_with(|| self.mobs.spawn_rng.below(MONSTER_WEIGHT) < ZOMBIE_WEIGHT);
                if !zombie {
                    break;
                }
                if !ground_ok(w, [px, y, pz], MobKind::Zombie, bufs) {
                    continue;
                }
                // isSpawnDark: небо > rand(32) — отказ; общий свет (с тьмой ночи) > U(0..7) — отказ.
                let sky = sky_light(w, px, y, pz);
                let r = &mut self.mobs.spawn_rng;
                if sky > r.below(32) {
                    continue;
                }
                let light = (sky - dark).max(0);
                if light > r.below(8) {
                    continue;
                }
                let baby = r.next_f32() < 0.05;
                if self.spawn_mob(MobKind::Zombie, pos, baby).is_some() {
                    made += 1;
                    if made >= 4 || made >= left {
                        return made;
                    }
                }
            }
        }
        made
    }
}

/// Толчок Entity.pushAwayFrom: сдвиг центров (b − a), m = max(|dx|, |dz|) ≥ 0.01 → /√m, /max(1, √m), ×0.05.
/// Ответ — вектор для b (a получает минус).
pub(crate) fn push_vec(a: [f64; 3], b: [f64; 3]) -> Option<(f64, f64)> {
    let (mut dx, mut dz) = (b[0] - a[0], b[2] - a[2]);
    let mut m = dx.abs().max(dz.abs());
    if m < 0.01 {
        return None;
    }
    m = m.sqrt();
    dx /= m;
    dz /= m;
    let inv = (1.0 / m).min(1.0);
    dx *= inv * 0.05;
    dz *= inv * 0.05;
    Some((dx, dz))
}
