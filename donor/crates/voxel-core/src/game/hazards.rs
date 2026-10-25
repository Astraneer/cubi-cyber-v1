//! Опасности мира для игрока (ASP-12 §3): кактус касанием, удушье головой в блоке. Запросы к миру без состояния; вода и
//! лава (касание, глаза) считает сама физика игрока (player.rs: update_fluids). Урон и окно — game/server/survival.rs.

use crate::game::player::Player;
use crate::registry::blocks::{bdef, collision_of};
use crate::registry::ids::CACTUS;
use crate::registry::shapes::Aabb;
use crate::world::WorldData;

/// Кактус касанием (Entity.checkBlockCollision): среди ячеек, которые пересекает коробка тела, сжатая на 1e-7, есть кактус.
/// Форма коллизии кактуса уже клетки на пиксель — стоя вплотную, коробка заходит в ячейку на 1/16.
pub(crate) fn cactus_touch(w: &mut WorldData, body: &Aabb) -> bool {
    let b = body.contract(1e-7);
    let c = |a: f64| a.floor() as i32;
    for z in c(b.lo[2])..=c(b.hi[2]) {
        for y in c(b.lo[1])..=c(b.hi[1]) {
            for x in c(b.lo[0])..=c(b.hi[0]) {
                if w.get_block(x, y, z) == CACTUS {
                    return true;
                }
            }
        }
    }
    false
}

/// Голова в блоке (Entity.isInsideWall): тонкая коробка 0.48 × 1e-6 × 0.48 в точке глаз пересекает форму коллизии
/// «удушающего» блока (полный непрозрачный; листва, стекло, кактус, сундук, факел не душат).
pub(crate) fn in_wall(w: &mut WorldData, p: &Player) -> bool {
    let ey = p.pos[1] + p.eye_height() as f64;
    let half = (0.6f32 * 0.8f32) as f64 / 2.0;
    let b = Aabb { lo: [p.pos[0] - half, ey - 5e-7, p.pos[2] - half], hi: [p.pos[0] + half, ey + 5e-7, p.pos[2] + half] };
    let c = |a: f64| a.floor() as i32;
    for z in c(b.lo[2])..=c(b.hi[2]) {
        for y in c(b.lo[1])..=c(b.hi[1]) {
            for x in c(b.lo[0])..=c(b.hi[0]) {
                if !bdef(w.get_block(x, y, z)).suffocates {
                    continue;
                }
                let cell = w.get_cell(x, y, z);
                let o = [x as f64, y as f64, z as f64];
                if collision_of(cell).iter().any(|s| s.moved(o).intersects(&b)) {
                    return true;
                }
            }
        }
    }
    false
}
