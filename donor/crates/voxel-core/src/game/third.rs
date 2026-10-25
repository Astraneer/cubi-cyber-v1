//! Вид от третьего лица (G16): камера за плечом не уходит в блоки (Camera.clipToSpace) и своя модель игрока — та же
//! поза, что у других игроков (ClientSim::player_draws): поворот тела, шаг ног, взмах. Смещение камеры, сглаживание и
//! переключатель — web/game/camera.js; модель — web/render/players.js.

use crate::game::interact::hit_boxes;
use crate::game::physics::Solids;
use crate::game::player::Player;
use crate::registry::shapes::Aabb;

/// Градусы в [−180, 180) (MathHelper.wrapDegrees).
pub(crate) fn wrap_deg(a: f32) -> f32 {
    let r = a.rem_euclid(360.0);
    if r >= 180.0 {
        r - 360.0
    } else {
        r
    }
}

/// Тело за тик (LivingEntity.tickMovement + turnHead, 1.21.1): идёт (сдвиг dx, dz за тик) — тело к направлению хода
/// (спиной вперёд, если взгляд почти назад), машет рукой — к взгляду; тянется на 0.3 от разницы за тик; голова от тела
/// не дальше 75°, а дальше 50° тело доворачивается к голове на 0.2 от угла. Ответ — новый поворот тела.
pub(crate) fn turn_body(body: f32, yaw: f32, dx: f32, dz: f32, swinging: bool) -> f32 {
    let mut target = body;
    if dx * dx + dz * dz > 0.0025 {
        let h = dz.atan2(dx).to_degrees() - 90.0;
        let i = (wrap_deg(yaw) - h).abs();
        target = if 95.0 < i && i < 265.0 { h - 180.0 } else { h };
    }
    if swinging {
        target = yaw;
    }
    let mut body = body + wrap_deg(target - body) * 0.3;
    let g = wrap_deg(yaw - body).clamp(-75.0, 75.0);
    body = yaw - g;
    if g * g > 2500.0 {
        body += g * 0.2;
    }
    body
}

/// Своя модель: поворот тела, шаг ног (LimbAnimator), возраст — по тикам своего игрока, как у Remote.
#[derive(Default)]
pub(crate) struct SelfBody {
    body: f32,
    body_prev: f32,
    limb: f32,
    limb_prev: f32,
    amt: f32,
    amt_prev: f32,
    age: u32,
}

impl SelfBody {
    /// Тик своего игрока закончен: ход prev → pos.
    pub(crate) fn tick(&mut self, p: &Player) {
        let (dx, dz) = ((p.pos[0] - p.prev[0]) as f32, (p.pos[2] - p.prev[2]) as f32);
        self.age = self.age.wrapping_add(1);
        self.body_prev = self.body;
        self.body = turn_body(self.body, p.yaw, dx, dz, p.swinging);
        let speed = (dx * dx + dz * dz).sqrt();
        self.amt_prev = self.amt;
        self.limb_prev = self.limb;
        self.amt += ((speed * 4.0).min(1.0) - self.amt) * 0.4;
        self.limb += self.amt;
    }
    /// Встать без отставания (вход, телепорт): тело по взгляду.
    pub(crate) fn snap(&mut self, yaw: f32) {
        self.body = yaw;
        self.body_prev = yaw;
    }
    /// 13 чисел, как одна запись `ClientSim::player_draws`: id, ступни (интерполяция), yaw°, pitch°, фаза и размах шага,
    /// взмах 0…1, флаги PM_*, поворот тела°, возраст, скин.
    pub(crate) fn draw(&self, p: &Player, alpha: f64, id: u32, flags: u8, skin: u8) -> [f32; 13] {
        let a = alpha as f32;
        let l = |x: f64, y: f64| (x + (y - x) * alpha) as f32;
        let (s0, s1) = (p.last_swing, p.swing);
        let mut d = s1 - s0;
        if d < 0.0 {
            d += 1.0;
        }
        let swing = s0 + d * a; // как взмах руки в game.view (ClientSim::view)
        [
            id as f32,
            l(p.prev[0], p.pos[0]),
            l(p.prev[1], p.pos[1]),
            l(p.prev[2], p.pos[2]),
            p.yaw,
            p.pitch,
            self.limb_prev + (self.limb - self.limb_prev) * a,
            self.amt_prev + (self.amt - self.amt_prev) * a,
            swing,
            flags as f32,
            self.body_prev + wrap_deg(self.body - self.body_prev) * a,
            self.age as f32 + a,
            skin as f32,
        ]
    }
}

/// Camera.clipToSpace: на сколько камера может отъехать от глаз eye по направлению dir (единичный) не дальше max — восемь
/// лучей из углов кубика ±0.1 вокруг глаз по формам коллизии (ShapeType.VISUAL у нас — коллизия: трава и цветы не мешают,
/// листва и стекло — да). В отличие от Minecraft (расстояние от глаз до точки попадания) — путь луча до попадания: кубик
/// камеры ±0.1 остаётся снаружи блоков, ближняя плоскость 0.05 стену не режет.
pub(crate) fn camera_clip(w: &mut impl Solids, eye: [f64; 3], dir: [f64; 3], max: f64) -> f64 {
    if !eye.iter().chain(dir.iter()).all(|v| v.is_finite()) || !(max > 0.0) {
        return 0.0;
    }
    let mut f = max;
    let mut boxes: Vec<Aabb> = Vec::with_capacity(4);
    for i in 0..8 {
        let o = [((i & 1) * 2 - 1) as f64 * 0.1, ((i >> 1 & 1) * 2 - 1) as f64 * 0.1, ((i >> 2 & 1) * 2 - 1) as f64 * 0.1];
        let a = [eye[0] + o[0], eye[1] + o[1], eye[2] + o[2]];
        let b = [a[0] + dir[0] * f, a[1] + dir[1] * f, a[2] + dir[2] * f];
        if let Some(p) = ray_shapes(w, a, b, &mut boxes) {
            let t: f64 = (0..3).map(|k| (p[k] - a[k]) * dir[k]).sum();
            f = f.min(t.max(0.0));
        }
    }
    f
}

/// Первая точка попадания отрезка a → b в формы коллизии (DDA по клеткам, как interact::raycast_ex).
fn ray_shapes(w: &mut impl Solids, a: [f64; 3], b: [f64; 3], boxes: &mut Vec<Aabb>) -> Option<[f64; 3]> {
    let d: [f64; 3] = std::array::from_fn(|k| b[k] - a[k]);
    if d.iter().all(|v| v.abs() < 1e-9) {
        return None;
    }
    let mut test = |c: [i32; 3], w: &mut dyn FnMut(&mut Vec<Aabb>, [i32; 3])| {
        boxes.clear();
        w(boxes, c);
        hit_boxes(c, boxes, a, b).map(|h| h.point)
    };
    let mut shapes = |out: &mut Vec<Aabb>, c: [i32; 3]| w.shapes_at(c[0], c[1], c[2], out);
    let mut c = [a[0].floor() as i32, a[1].floor() as i32, a[2].floor() as i32];
    if let Some(p) = test(c, &mut shapes) {
        return Some(p);
    }
    let step: [i32; 3] = std::array::from_fn(|k| if d[k] > 0.0 { 1 } else if d[k] < 0.0 { -1 } else { 0 });
    let td: [f64; 3] = std::array::from_fn(|k| if step[k] == 0 { f64::MAX } else { step[k] as f64 / d[k] });
    let mut tm: [f64; 3] = std::array::from_fn(|k| {
        let fr = a[k] - a[k].floor();
        td[k] * if step[k] > 0 { 1.0 - fr } else { fr }
    });
    for _ in 0..64 {
        if !(tm[0] <= 1.0 || tm[1] <= 1.0 || tm[2] <= 1.0) {
            break;
        }
        let k = if tm[0] < tm[1] {
            if tm[0] < tm[2] {
                0
            } else {
                2
            }
        } else if tm[1] < tm[2] {
            1
        } else {
            2
        };
        c[k] += step[k];
        tm[k] += td[k];
        if let Some(p) = test(c, &mut shapes) {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Стена x ≥ 2 (сплошной кусок мира), остальное пусто.
    struct Wall;
    impl Solids for Wall {
        fn shapes_at(&mut self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if x >= 2 {
                out.push(Aabb { lo: [x as f64, y as f64, z as f64], hi: [x as f64 + 1.0, y as f64 + 1.0, z as f64 + 1.0] });
            }
        }
        fn slip(&mut self, _: i32, _: i32, _: i32) -> f32 {
            0.6
        }
        fn chokes(&mut self, _: i32, _: i32, _: i32) -> bool {
            false
        }
    }

    #[test]
    fn camera_stops_before_wall() {
        // Глаза в 1.5 от стены, камера хочет отъехать на 4 к стене: остановка у стены минус угол кубика 0.1.
        let f = camera_clip(&mut Wall, [0.5, 64.6, 0.5], [1.0, 0.0, 0.0], 4.0);
        assert!((f - 1.4).abs() < 1e-6, "{f}");
        // От стены — свободно на все 4.
        let g = camera_clip(&mut Wall, [0.5, 64.6, 0.5], [-1.0, 0.0, 0.0], 4.0);
        assert_eq!(g, 4.0);
    }

    #[test]
    fn body_follows_walk_like_remote() {
        let mut p = Player::new([0.0, 64.0, 0.0]);
        let mut b = SelfBody::default();
        p.yaw = 0.0;
        for _ in 0..20 {
            p.prev = p.pos;
            p.pos[0] -= 0.2; // идёт на −X (yaw 90) боком, взгляд на +Z
            b.tick(&p);
        }
        let v = b.draw(&p, 1.0, 7, 0, 3);
        assert!(v[10] > 40.0 && v[10] <= 75.0, "тело {}", v[10]); // повернулось к ходу, но голова не дальше 75°
        assert!(v[7] > 0.5, "размах шага {}", v[7]);
        assert_eq!((v[0], v[12]), (7.0, 3.0));
    }
}
