//! Выживание на сервере (срез 4, ASP-12): здоровье игрока считает только сервер (Auth) — урон падения, кактуса, стены,
//! утопления, лавы и горения, пустоты; окно неуязвимости, броня и её износ; регенерация [наше]; смерть с выпадением всех
//! вещей; возрождение на точке появления (`spawn_point`). Клиент не предсказывает урон (как Minecraft): ему идёт
//! Msg::Vitals при каждом изменении, звуки и частицы — события Ev::Hurt / Death / Burn / Bubbles.
//!
//! Порядок тика игрока (ASP-12 §1): `hp_pre` (счётчики −1, опасности baseTick: пустота, стена, воздух, огонь и лава) →
//! движение (`shadow` — теневой тик той же физики) → `hp_post` (урон падения по приземлению этого тика, кактус,
//! регенерация). Подмодуль server.rs: пользуется его закрытыми полями.

use super::*;
use crate::abi::{Ev, Sfx, EV_TOOL_BROKE};
use crate::game::hazards;
use crate::game::health::{death_text, fall_damage_k, DamageKind, Health, Vitals, BURN_TICKS, RESPAWN_AFTER, VOID_Y};
use crate::game::equip::equip_from_hand;
use crate::registry::ids::{FARMLAND, HAY_BLOCK};
use crate::registry::items::{armor_toughness, equip_slot, food_nutrition};


/// Поедание на сервере: тиков осталось, ячейка и предмет, номер UseAir клиента.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Eating {
    pub(crate) left: i32,
    pub(crate) slot: usize,
    pub(crate) item: u16,
    pub(crate) seq: u32,
}

/// Время еды, тиков.
const EAT_TICKS: i32 = 32;

/// Очки брони надетых частей (сумма, зажим 0…30).
pub(crate) fn armor_points(inv: &Inventory) -> f32 {
    let sum: i32 = inv.armor.iter().map(|s| if s.is_empty() { 0 } else { if let ItemKind::Armor { points, .. } = idef(s.id()).kind { points as i32 } else { 0 } }).sum();
    sum.clamp(0, 30) as f32
}

/// Стойкость надетых частей (attribute armor_toughness): алмазная и стальная броня — по 2 на часть.
pub(crate) fn armor_toughness_sum(inv: &Inventory) -> f32 {
    inv.armor.iter().map(|s| armor_toughness(s.id())).sum()
}

/// Своё состояние для клиента.
pub(crate) fn vitals_of(p: &ServerPlayer) -> Vitals {
    let h = &p.hp;
    Vitals {
        hp: h.hp,
        air: h.air.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        armor: armor_points(&p.inv) as u8,
        burning: h.fire > 0,
        dead: !h.alive(),
        death_time: h.death_time.clamp(0, 255) as u8,
        tilt: h.tilt_yaw,
        cause: if h.alive() { 0 } else { p.death_kind.map_or(0, |k| k.code()) },
    }
}

impl ServerSim {
    /// Отладочный полёт сквозь стены с отладкой — «творческий»: неуязвим ко всему, кроме пустоты. Полёт выживания (F) —
    /// обычный: урон, огонь, вода.
    fn creative(&self, i: usize) -> bool {
        let p = &self.players[i].player;
        (self.debug && p.noclip) || crate::game::rules::invulnerable(p) // творческий и наблюдатель (G17)
    }

    /// Урон игроку i (LivingEntity.damage): окно, броня и её износ, события. true — урон прошёл. Смерть — `die`.
    pub(crate) fn hurt(&mut self, i: usize, kind: DamageKind, amount: f32) -> bool {
        self.hurt_hit(i, kind, amount, None).is_some()
    }

    /// То же с исходом (`Hit`: полный ли удар, умер ли) и номером бьющего игрока (для сообщения о смерти). Удар игрока
    /// режет защита PvP (`pvp_shielded`: зона Перекрёстка + 10 с после выхода, без метки боя), прочий урон — сама зона.
    pub(crate) fn hurt_hit(&mut self, i: usize, kind: DamageKind, amount: f32, by: Option<u32>) -> Option<crate::game::health::Hit> {
        // Зона Перекрёстка (spawn.md): урона нет (пустота пробивает).
        if kind == DamageKind::PlayerAttack {
            if self.pvp_shielded(i) {
                return None;
            }
        } else if kind != DamageKind::OutOfWorld {
            let feet = crate::game::interact::feet(&self.players[i].player);
            if crate::game::interact::protected(&self.sim.world.d.borrow(), feet) {
                return None;
            }
        }
        let creative = self.creative(i);
        let armor = armor_points(&self.players[i].inv);
        let toughness = armor_toughness_sum(&self.players[i].inv);
        let hit = self.players[i].hp.damage_t(kind, amount, armor, toughness, creative)?;
        let feet = self.players[i].feet_cell();
        if hit.wear > 0 {
            self.wear_armor(i, hit.wear);
        }
        if hit.died {
            self.die(i, kind, by);
        } else if hit.full {
            // Урон слышат все рядом (sound.md): вода — «бульканье», остальное — глухой удар; падение само звучит из Ev::Landed.
            let sfx = if kind == DamageKind::Drown { Sfx::HurtDrown } else { Sfx::Hurt };
            self.events.push(Ev::Sound as i32, feet, sfx as i32);
        }
        Some(hit)
    }

    /// Износ каждой надетой части (PlayerEntity.damageArmor): сломалась — исчезает со звуком.
    fn wear_armor(&mut self, i: usize, n: i32) {
        let feet = self.players[i].feet_cell();
        for k in 0..4 {
            let s = &mut self.players[i].inv.armor[k];
            if s.is_empty() {
                continue;
            }
            let max = idef(s.id()).max_damage as i32;
            s.damage = (s.damage as i32 + n).min(u16::MAX as i32) as u16;
            if max > 0 && s.damage as i32 >= max {
                let id = s.id() as i32;
                *s = ItemStack::EMPTY;
                self.events.push(EV_TOOL_BROKE, feet, id);
            }
        }
    }

    /// Смерть (ServerPlayerEntity.onDeath, keepInventory = false): окно закрывается (курсор и сетка возвращаются в
    /// инвентарь), все вещи, включая броню и вторую руку, выпадают у тела — каждый стек со своей скоростью; сообщение
    /// о смерти — всем в чат.
    pub(crate) fn die(&mut self, i: usize, kind: DamageKind, by: Option<u32>) {
        self.close_screen(i, false);
        self.stop_breaking(i);
        self.players[i].eating = None;
        let id = self.players[i].id;
        let feet = self.players[i].feet_cell();
        let (pos, eye, yaw, pitch) = {
            let p = &self.players[i].player;
            (p.pos, p.eye_height(), p.yaw, p.pitch)
        };
        let order: Vec<usize> = (0..36).chain(36..40).chain(std::iter::once(OFFHAND)).collect();
        for slot in order {
            let s = self.players[i].inv.get(slot);
            if s.is_empty() {
                continue;
            }
            self.players[i].inv.set(slot, ItemStack::EMPTY);
            let sim = &mut self.sim;
            sim.items.throw(pos, eye, yaw, pitch, s, true, false, &mut sim.rng, &mut sim.ids);
        }
        self.players[i].inv_force = true;
        self.players[i].death_kind = Some(kind);
        self.events.push(Ev::Sound as i32, feet, Sfx::Death as i32);
        let text = match by.and_then(|b| self.player(b)).map(|k| k.nick.clone()) {
            Some(killer) if kind == DamageKind::PlayerAttack => format!("{} был убит {killer}", self.players[i].nick),
            _ => death_text(&self.players[i].nick, kind),
        };
        self.announce(CHAT_DEATH, id, text);
    }

    /// Начало тика игрока i (до движения): окно −1, опасности Entity/LivingEntity.baseTick по позиции начала тика.
    pub(crate) fn hp_pre(&mut self, i: usize) {
        let d = self.sim.world.d.clone();
        // Касание воды и лавы, глаза в воде — по позиции начала тика (читаем, не меняя тело: его считает теневой тик).
        let (in_water, in_lava, eye_water) = {
            let mut w = d.borrow_mut();
            let p = &mut self.players[i].player;
            if p.noclip {
                (false, false, false)
            } else {
                let (wet, lava, eye) = p.probe_fluids(&mut *w);
                (wet.is_some(), lava.is_some(), eye)
            }
        };
        self.players[i].hp.begin();
        if self.players[i].hp.alive() {
            let creative = self.creative(i);
            let pos = self.players[i].player.pos;
            if pos[1] < VOID_Y {
                self.hurt(i, DamageKind::OutOfWorld, 4.0);
            }
            let wall = hazards::in_wall(&mut *d.borrow_mut(), &self.players[i].player);
            if wall {
                self.hurt(i, DamageKind::InWall, 1.0);
            }
            // Воздух: творческому не тратится (abilities.invulnerable).
            if let Some(dmg) = self.players[i].hp.tick_air(eye_water && !creative) {
                self.hurt(i, DamageKind::Drown, dmg);
            }
            // Горение, затем лава (Entity.baseTick): вода тушит; лава поджигает на 15 с и бьёт на 4 (падение в лаве вдвое гаснет
            // в самой физике игрока: update_fluids).
            if creative {
                self.players[i].hp.fire = 0;
            } else if let Some(dmg) = self.players[i].hp.tick_fire(in_water, in_lava) {
                self.hurt(i, DamageKind::OnFire, dmg);
            }
            if in_lava && !creative {
                self.players[i].hp.ignite(BURN_TICKS);
                self.hurt(i, DamageKind::Lava, 4.0);
            }
        }
        if self.players[i].hp.base_tick_end() {
            // 20-й тик смерти: тело убирается (чужим клиентам — флаг смерти в PlayerMove); вещи уже выпали.
        }
        self.eat_tick(i);
    }

    /// ПКМ с едой (Act::UseAir): начать есть, если в главной руке еда и сердца не полны [наше]. Иначе — отказ: клиент
    /// получает UseStop по тому же seq и снимает «ем».
    pub(crate) fn use_air(&mut self, i: usize, hand: u8, seq: u32) {
        // Броня в руке — надеть (ArmorItem.use), а не есть.
        let held = if hand == 0 { self.players[i].inv.hand() } else { self.players[i].inv.offhand };
        if equip_slot(held.id()).is_some() {
            self.auth(i, |c| equip_from_hand(c, hand as usize));
            return self.ack(i, seq);
        }
        let p = &self.players[i];
        let s = p.inv.hand();
        let ok = hand == 0 && p.eating.is_none() && p.hp.can_eat() && food_nutrition(s.id()).is_some();
        if !ok {
            if p.eating.is_none() {
                self.players[i].own.push(Msg::UseStop { seq });
            }
            return self.ack(i, seq);
        }
        let p = &mut self.players[i];
        p.eating = Some(Eating { left: EAT_TICKS, slot: p.inv.selected, item: s.id(), seq });
        self.ack(i, seq);
    }

    /// Прервать поедание (отпустил ПКМ, сменил предмет, умер). notify — сказать клиенту (UseStop по его seq).
    pub(crate) fn eat_cancel(&mut self, i: usize, notify: Option<u32>) {
        let p = &mut self.players[i];
        if let (Some(e), Some(_)) = (p.eating.take(), notify) {
            p.own.push(Msg::UseStop { seq: e.seq });
        }
    }

    /// Тик поедания (LivingEntity.tickItemStackUsage, до движения): предмет в руке сменился — прервать; крошки и звук еды
    /// на 8-м, 12-м… тике (другим); −1; на 0 — съесть: стек −1, лечение на питание, звуки еды и отрыжки (другим).
    fn eat_tick(&mut self, i: usize) {
        let Some(mut e) = self.players[i].eating else { return };
        let id = self.players[i].id;
        let feet = self.players[i].feet_cell();
        let s = self.players[i].inv.hand();
        if s.is_empty() || s.id() != e.item || self.players[i].inv.selected != e.slot || !self.players[i].hp.alive() {
            self.eat_cancel(i, Some(e.seq));
            return;
        }
        if EAT_TICKS - e.left > 7 && e.left % 4 == 0 {
            self.events.push_to(To::Except(id), Ev::Sound as i32, feet, Sfx::Eat as i32);
        }
        e.left -= 1;
        if e.left > 0 {
            self.players[i].eating = Some(e);
            return;
        }
        let p = &mut self.players[i];
        p.eating = None;
        p.own.push(Msg::UseStop { seq: e.seq });
        if let Some(nutrition) = food_nutrition(s.id()) {
            let slot = p.inv.selected;
            p.inv.set(slot, s.with_count(s.n() - 1));
            p.hp.heal(nutrition as f32);
        }
        self.events.push_to(To::Except(id), Ev::Sound as i32, feet, Sfx::EatDone as i32);
        self.events.push_to(To::Except(id), Ev::Sound as i32, feet, Sfx::Burp as i32);
    }

    /// Конец тика игрока i (после движения): урон падения по приземлению этого тика, кактус, регенерация.
    pub(crate) fn hp_post(&mut self, i: usize) {
        if !self.players[i].hp.alive() {
            return;
        }
        let landed = self.players[i].player.landed;
        if landed != self.players[i].fall_seen {
            self.players[i].fall_seen = landed;
            if let Some((_, fd)) = landed {
                // Block.onLandedUpon по блоку опоры (floor(y − 0.2)): пашню топчут, на стог урон ×0.2.
                let [px, py, pz] = self.players[i].player.pos;
                let land = [px.floor() as i32, (py - 0.2).floor() as i32, pz.floor() as i32];
                let d = self.sim.world.d.clone();
                let under = d.borrow_mut().get_block(land[0], land[1], land[2]);
                let mut k = 1.0f32;
                if under == FARMLAND {
                    self.sim.land_on_farmland(&mut d.borrow_mut(), land, fd, true, &mut self.events);
                } else if under == HAY_BLOCK {
                    k = 0.2;
                }
                let dmg = fall_damage_k(fd, k);
                if dmg > 0 {
                    self.hurt(i, DamageKind::Fall, dmg as f32);
                }
            }
        }
        let cactus = {
            let d = self.sim.world.d.clone();
            let mut w = d.borrow_mut();
            hazards::cactus_touch(&mut *w, &self.players[i].player.bbox())
        };
        if cactus {
            self.hurt(i, DamageKind::Cactus, 1.0);
        }
        self.players[i].hp.regen();
    }

    /// C2S::Respawn: кнопка экрана смерти (активна с 20-го тика смерти). Живому — ничего.
    pub(crate) fn respawn(&mut self, i: usize) {
        let h = &self.players[i].hp;
        if h.alive() || h.death_time < RESPAWN_AFTER {
            return;
        }
        self.reset_after_death(i);
    }

    /// Новое тело на Перекрёстке: полное здоровье и воздух, 60 тиков неуязвимости, пустой инвентарь (вещи выпали), первый
    /// слот; перенос и Teleport клиенту (до TeleportAck ходы не принимаются) — `respawn_at_crossroads` команды spawn.
    pub(crate) fn reset_after_death(&mut self, i: usize) {
        let p = &mut self.players[i];
        p.hp = Health::new();
        p.death_kind = None;
        p.eating = None;
        p.pvp_tag = 0;
        p.player.pitch = 0.0;
        p.inv.selected = 0;
        p.inv_force = true;
        // Новое тело не летит (G16): клиенту — до Teleport, ходы до TeleportAck не принимаются.
        if p.player.flying && !p.player.noclip {
            p.player.set_flight(false);
            p.own.push(Msg::Flight(false));
        }
        self.respawn_at_crossroads(i);
        let p = &mut self.players[i];
        p.fall_seen = p.player.landed;
    }
}
