//! Коллизия AABB как в Java 1.21.1 (ASP-02 §3.4–3.9): формы блоков, ось Y первой, затем бо́льшая из X/Z, ступенька 0.6
//! по границам форм, защита края при приседе. Один решатель на тело (урок cubi-core) — игрок и (позже) мобы.
//! Мир для физики (Solids for WorldData) — формы коллизии и свойства блоков из таблицы.

use crate::cell::{block_of, H};
use crate::game::fluids::fluid_height;
use crate::registry::blocks::{bdef, collision_of};
use crate::registry::shapes::{Aabb, CUBE};
use crate::world::WorldData;

/// Допуск форм (VoxelShape.calculateMaxDistance, VoxelShapes.calculateMaxOffset).
pub(crate) const EPS: f64 = 1e-7;
/// Что физике нужно от мира (E2): формы коллизии клетки, скользкость, удушающие блоки.
pub(crate) trait Solids {
    /// Формы коллизии клетки в мировых координатах — дописать в out.
    fn shapes_at(&mut self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>);
    /// Скользкость блока (0.6 по умолчанию, лёд 0.98).
    fn slip(&mut self, x: i32, y: i32, z: i32) -> f32;
    /// Удушающий блок (полный непрозрачный) — для выталкивания игрока.
    fn chokes(&mut self, x: i32, y: i32, z: i32) -> bool;
    /// Можно лезть (тег #climbable: лестница) — блок в клетке ступней (LivingEntity.isClimbing).
    fn climbable(&mut self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
    /// Жидкость в клетке: (род — registry::blocks::FLUID_*, 0 — нет; высота поверхности: 1.0 под такой же жидкостью,
    /// иначе 8/9 — FlowableFluid.getHeight источника).
    fn fluid(&mut self, _x: i32, _y: i32, _z: i32) -> (u8, f64) {
        (0, 0.0)
    }
    /// Вектор течения жидкости в клетке (FlowableFluid.getVelocity): единичный или нулевой, X/Y/Z.
    fn flow(&mut self, _x: i32, _y: i32, _z: i32) -> [f64; 3] {
        [0.0; 3]
    }
}

/// Все формы, пересекающие area (BlockCollisionSpliterator). Формы у нас не выше куба — рамку клеток не берём.
pub(crate) fn collisions(w: &mut impl Solids, area: &Aabb, out: &mut Vec<Aabb>) {
    out.clear();
    let lo = |a: usize| (area.lo[a] - EPS).floor() as i32;
    let hi = |a: usize| (area.hi[a] + EPS).floor() as i32;
    for z in lo(2)..=hi(2) {
        for y in lo(1)..=hi(1) {
            for x in lo(0)..=hi(0) {
                let n = out.len();
                w.shapes_at(x, y, z, out);
                let mut k = n;
                for i in n..out.len() {
                    if out[i].intersects(area) {
                        out[k] = out[i];
                        k += 1;
                    }
                }
                out.truncate(k);
            }
        }
    }
}

/// Сколько можно пройти по оси a от коробки b, не входя в форму s (одна коробка формы).
pub(crate) fn max_dist(a: usize, s: &Aabb, b: &Aabb, d: f64) -> f64 {
    if d.abs() < EPS {
        return 0.0;
    }
    for o in 0..3 {
        if o != a && !(b.hi[o] - EPS >= s.lo[o] && b.lo[o] + EPS < s.hi[o]) {
            return d; // не перекрывает по другим осям
        }
    }
    if d > 0.0 {
        if b.hi[a] - EPS < s.lo[a] {
            let f = s.lo[a] - b.hi[a];
            if f >= -EPS {
                return d.min(f);
            }
        }
    } else if b.lo[a] + EPS >= s.hi[a] {
        let f = s.hi[a] - b.lo[a];
        if f <= EPS {
            return d.max(f);
        }
    }
    d // уже внутри формы глубже допуска — форма не держит
}

pub(crate) fn max_offset(a: usize, b: &Aabb, shapes: &[Aabb], mut d: f64) -> f64 {
    for s in shapes {
        if d.abs() < EPS {
            return 0.0;
        }
        d = max_dist(a, s, b, d);
    }
    d
}

/// Перемещение m против форм: Y, потом бо́льшая по модулю из X/Z (при равенстве — X), потом другая.
pub(crate) fn sweep(m: [f64; 3], mut b: Aabb, shapes: &[Aabb]) -> [f64; 3] {
    if shapes.is_empty() {
        return m;
    }
    let [mut dx, mut dy, mut dz] = m;
    if dy != 0.0 {
        dy = max_offset(1, &b, shapes, dy);
        if dy != 0.0 {
            b = b.moved([0.0, dy, 0.0]);
        }
    }
    let z_first = dx.abs() < dz.abs();
    if z_first && dz != 0.0 {
        dz = max_offset(2, &b, shapes, dz);
        if dz != 0.0 {
            b = b.moved([0.0, 0.0, dz]);
        }
    }
    if dx != 0.0 {
        dx = max_offset(0, &b, shapes, dx);
        if !z_first && dx != 0.0 {
            b = b.moved([dx, 0.0, 0.0]);
        }
    }
    if !z_first && dz != 0.0 {
        dz = max_offset(2, &b, shapes, dz);
    }
    [dx, dy, dz]
}

#[inline]
pub(crate) fn hlen2(v: [f64; 3]) -> f64 {
    v[0] * v[0] + v[2] * v[2]
}

/// Буферы форм, чтобы не выделять память каждый тик.
#[derive(Default)]
pub(crate) struct Bufs {
    pub(crate) a: Vec<Aabb>,
    pub(crate) b: Vec<Aabb>,
}

/// Entity.adjustMovementForCollisions со ступенькой 1.21.1: если упёрлись по горизонтали на земле (или в тике
/// приземления) — кандидаты высоты из границ форм ≤ step, по возрастанию; первый, дающий больший путь, — ответ.
pub(crate) fn collide(w: &mut impl Solids, b: &Aabb, m: [f64; 3], on_ground: bool, step: f32, bufs: &mut Bufs) -> [f64; 3] {
    if m == [0.0; 3] {
        return m;
    }
    collisions(w, &b.stretch(m), &mut bufs.a);
    let v = sweep(m, *b, &bufs.a);
    let (bx, by, bz) = (m[0] != v[0], m[1] != v[1], m[2] != v[2]);
    let landing = by && m[1] < 0.0;
    if step > 0.0 && (landing || on_ground) && (bx || bz) {
        let b2 = if landing { b.moved([0.0, v[1], 0.0]) } else { *b };
        let mut b3 = b2.stretch([m[0], step as f64, m[2]]);
        if !landing {
            b3 = b3.stretch([0.0, -(1e-5f32 as f64), 0.0]);
        }
        collisions(w, &b3, &mut bufs.b);
        let fy = v[1] as f32;
        let mut cands: Vec<f32> = Vec::with_capacity(4);
        for s in &bufs.b {
            for d in [s.lo[1], s.hi[1]] {
                let g = (d - b2.lo[1]) as f32;
                if !(g < 0.0) && g != fy && g <= step && !cands.contains(&g) {
                    cands.push(g);
                }
            }
        }
        cands.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for g in cands {
            let v2 = sweep([m[0], g as f64, m[2]], b2, &bufs.b);
            if hlen2(v2) > hlen2(v) {
                let dd = b.lo[1] - b2.lo[1];
                return [v2[0], v2[1] - dd, v2[2]];
            }
        }
    }
    v
}

/// Пусто ли под коробкой, сдвинутой на (dx, dz), на глубину f (PlayerEntity.isSpaceAroundPlayerEmpty).
pub(crate) fn space_below_empty(w: &mut impl Solids, b: &Aabb, dx: f64, dz: f64, f: f32, buf: &mut Vec<Aabb>) -> bool {
    let q = Aabb {
        lo: [b.lo[0] + dx, b.lo[1] - f as f64 - (1e-5f32 as f64), b.lo[2] + dz],
        hi: [b.hi[0] + dx, b.lo[1], b.hi[2] + dz],
    };
    collisions(w, &q, buf);
    buf.is_empty()
}

/// Защита края при приседе (PlayerEntity.adjustMovementForSneaking): X и Z урезаются шагом 0.05, пока под сдвинутой
/// коробкой пусто на глубину шага; затем обе вместе (внешний угол). Тот же след коробки, что у коллизии.
pub(crate) fn edge_guard(w: &mut impl Solids, b: &Aabb, m: [f64; 3], step: f32, buf: &mut Vec<Aabb>) -> [f64; 3] {
    let (mut d, mut e) = (m[0], m[2]);
    let si = if d != 0.0 { 0.05f64.copysign(d) } else { 0.0 };
    let sj = if e != 0.0 { 0.05f64.copysign(e) } else { 0.0 };
    while d != 0.0 && space_below_empty(w, b, d, 0.0, step, buf) {
        if d.abs() <= 0.05 {
            d = 0.0;
            break;
        }
        d -= si;
    }
    while e != 0.0 && space_below_empty(w, b, 0.0, e, step, buf) {
        if e.abs() <= 0.05 {
            e = 0.0;
            break;
        }
        e -= sj;
    }
    while d != 0.0 && e != 0.0 && space_below_empty(w, b, d, e, step, buf) {
        d = if d.abs() <= 0.05 { 0.0 } else { d - si };
        e = if e.abs() <= 0.05 { 0.0 } else { e - sj };
    }
    [d, m[1], e]
}

/// Пусто ли в коробке (CollisionView.isSpaceEmpty).
pub(crate) fn space_empty(w: &mut impl Solids, b: &Aabb, buf: &mut Vec<Aabb>) -> bool {
    collisions(w, b, buf);
    buf.is_empty()
}

/// Мир для физики: формы коллизии по таблице блоков; ниже y = 0 — твёрдо, выше 127 — воздух.
impl Solids for WorldData {
    fn shapes_at(&mut self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        let o = [x as f64, y as f64, z as f64];
        if y < 0 {
            out.push(CUBE[0].moved(o));
            return;
        }
        if y >= H {
            return;
        }
        for s in collision_of(self.get_cell(x, y, z)) {
            out.push(s.moved(o));
        }
    }
    fn slip(&mut self, x: i32, y: i32, z: i32) -> f32 {
        bdef(self.get_block(x, y, z)).slip
    }
    fn chokes(&mut self, x: i32, y: i32, z: i32) -> bool {
        bdef(self.get_block(x, y, z)).suffocates
    }
    fn climbable(&mut self, x: i32, y: i32, z: i32) -> bool {
        bdef(self.get_block(x, y, z)).climbable // тег #climbable: из нашего объёма — лестница
    }
    fn fluid(&mut self, x: i32, y: i32, z: i32) -> (u8, f64) {
        if y < 0 || y >= H {
            return (0, 0.0);
        }
        let c = self.get_cell(x, y, z);
        let f = bdef(block_of(c)).fluid;
        if f == 0 {
            return (0, 0.0);
        }
        let above = if y + 1 < H { self.get_cell(x, y + 1, z) } else { 0 };
        (f, fluid_height(c, above)) // уровень течения: источник 8/9, течёт — количество/9, под такой же жидкостью — 1
    }
    fn flow(&mut self, x: i32, y: i32, z: i32) -> [f64; 3] {
        crate::game::fluids::fluid_velocity(self, [x, y, z])
    }
}
