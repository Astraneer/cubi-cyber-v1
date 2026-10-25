//! Звук в ядре (ASP-13): ритм шагов (Entity.move → distanceTraveled / nextStepSoundDistance) и выбор блока под ногами.
//! Сами звуки — в JS (web/audio/*): ядро только говорит «шаг по такому-то блоку здесь» событием `Ev::Step`.
//! Состояние шагов — часть вида (не симуляции): в отпечаток повтора не входит и не сохраняется.

use crate::abi::{EV_STEP, STEP_SECOND};
use crate::cell::BlockId;
use crate::game::view::Events;
use crate::registry::blocks::{bdef, Snd};
use crate::registry::ids::{AIR, SNOW_LAYER};
use crate::world::WorldData;

/// Счётчик шагов одного тела (свой игрок или чужой по его ходам).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Steps {
    /// Пройдено × 0.6 (f32, как distanceTraveled).
    dist: f32,
    /// Порог следующего шага.
    next: f32,
}

/// Что известно о теле за этот тик.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Body {
    /// Ступни после хода и сдвиг за тик.
    pub(crate) pos: [f64; 3],
    pub(crate) moved: [f64; 3],
    pub(crate) on_ground: bool,
    /// Лезет по лестнице (вертикаль идёт в путь, шаг звучит и без земли).
    pub(crate) climbing: bool,
    /// Присед на земле / полёт: шагов нет (getMoveEffect = NONE).
    pub(crate) silent: bool,
    /// Присед с Δy = 0 (шаг и в позе приседа без земли — у края).
    pub(crate) crouching: bool,
    /// Опорный блок (Entity.supportingBlockPos), если есть.
    pub(crate) support: Option<[i32; 3]>,
}

impl Steps {
    pub(crate) fn new() -> Steps {
        Steps { dist: 0.0, next: 1.0 }
    }

    /// Шаг этого тика: до двух событий (блок и второй шаг опоры). Порядок — Entity.move шаги 11–14.
    pub(crate) fn tick(&mut self, b: &Body, w: &mut WorldData, ev: &mut Events) {
        if b.silent {
            return;
        }
        let dy = if b.climbing { b.moved[1] } else { 0.0 };
        let d = (b.moved[0] * b.moved[0] + dy * dy + b.moved[2] * b.moved[2]).sqrt() as f32;
        self.dist += d * 0.6;
        if self.dist <= self.next {
            return;
        }
        let (x, z) = (b.pos[0].floor() as i32, b.pos[2].floor() as i32);
        // stepping = клетка под ступнями на 1e-5 (в прыжке — воздух: шагов нет, порог стоит).
        let stepping = w.get_block(x, (b.pos[1] - 1e-5).floor() as i32, z);
        if stepping == AIR {
            return;
        }
        let grounded = b.on_ground || b.climbing || (b.crouching && b.moved[1] == 0.0);
        if !grounded {
            return;
        }
        self.next = self.dist.floor() + 1.0;
        // landing = опора (на краю звучит тот блок, на котором реально стоишь) или клетка на 0.2 ниже.
        let at = b.support.unwrap_or([x, (b.pos[1] - 0.2).floor() as i32, z]);
        let feet = [x, (b.pos[1] + 1e-4).floor() as i32, z]; // 63.99999 на полу y = 64 — всё равно клетка 64
        for (block, flags) in step_blocks(w, at) {
            ev.push(EV_STEP, feet, block as i32 | flags << 16);
        }
    }
}

/// Блоки, звучащие шагом по клетке опоры: сам блок; снежный слой над ней — его шаг (0.15) и второй шаг опоры (0.05).
fn step_blocks(w: &mut WorldData, at: [i32; 3]) -> Vec<(BlockId, i32)> {
    let b = w.get_block(at[0], at[1], at[2]);
    let above = w.get_block(at[0], at[1] + 1, at[2]);
    if above == SNOW_LAYER {
        return vec![(above, 0), (b, STEP_SECOND)];
    }
    if bdef(b).sound == Snd::None {
        return vec![];
    }
    vec![(b, 0)]
}

/// Звуковая группа блока для JS (api::block_sound): номер Snd.
pub(crate) fn block_sound(b: BlockId) -> u8 {
    match bdef(b).sound {
        Snd::None => 0,
        Snd::Stone => 1,
        Snd::Wood => 2,
        Snd::Gravel => 3,
        Snd::Grass => 4,
        Snd::Sand => 5,
        Snd::Snow => 6,
        Snd::Wool => 7,
        Snd::Glass => 8,
        Snd::Metal => 9,
        Snd::Ladder => 10,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ids::STONE;
    use crate::world::World;

    fn body(moved: f64) -> Body {
        Body { pos: [0.5, 64.0, 0.5], moved: [moved, 0.0, 0.0], on_ground: true, climbing: false, silent: false, crouching: false, support: None }
    }

    #[test]
    fn walking_pace_matches_aspect() {
        // ASP-13 §2.1: ходьба 4.31718 б/с = 0.215859 б/тик → 2.59 шага в секунду; бег 5.61234 → 3.37.
        let world = World::new(1);
        let mut w = world.d.borrow_mut();
        for (speed, want) in [(0.215859, 2.59), (0.280617, 3.37)] {
            let mut st = Steps::new();
            let mut ev = Events::default();
            let n = 2000;
            for _ in 0..n {
                // Каменный пол под ногами: клетка y = 63.
                w.set_cell(0, 63, 0, STONE as u32);
                st.tick(&body(speed), &mut w, &mut ev);
            }
            let steps = ev.take().chunks(5).filter(|e| e[0] == EV_STEP).count();
            let per_s = steps as f64 / (n as f64 / 20.0);
            assert!((per_s - want).abs() < 0.03, "скорость {speed}: {per_s} шагов/с, ждали {want}");
        }
    }

    #[test]
    fn silent_in_air_and_when_sneaking() {
        let world = World::new(1);
        let mut w = world.d.borrow_mut();
        w.set_cell(0, 63, 0, STONE as u32);
        let mut ev = Events::default();
        let mut st = Steps::new();
        let mut b = body(0.2);
        b.on_ground = false; // прыжок: путь копится, шага нет
        for _ in 0..40 {
            st.tick(&b, &mut w, &mut ev);
        }
        assert!(ev.take().is_empty());
        b.on_ground = true; // приземлились: один шаг сразу, сколько бы порогов ни перешли
        st.tick(&b, &mut w, &mut ev);
        assert_eq!(ev.take().chunks(5).count(), 1);
        b.silent = true;
        for _ in 0..40 {
            st.tick(&b, &mut w, &mut ev);
        }
        assert!(ev.take().is_empty());
    }
}
