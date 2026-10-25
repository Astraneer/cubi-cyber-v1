//! PvP у клиента (G10): прицел по другим игрокам и удар ЛКМ. Игроки и мобы — одна «цель удара»: ближайшая из них на
//! луче взгляда (не дальше 3 блоков и ближе блока под прицелом) снимает выбор блока (GameRenderer.updateTargetedEntity).
//! Удар — Act::Attack с номером сущности и взмах руки (предсказание); урон, перезарядку, дальность и видимость решает
//! сервер (game/server/pvp.rs), клиент урон не предсказывает.

use crate::game::client::ClientSim;
use crate::game::interact::ENTITY_REACH;
use crate::game::mob::ai::seg_box_t;
use crate::proto::{Act, PM_CROUCH, PM_DEAD};
use crate::registry::shapes::Aabb;
use crate::world::WorldData;

/// Коробка тела игрока по ступням и позе: 0.6 × 1.8 (присед 1.5).
pub(crate) fn body_box(feet: [f64; 3], crouch: bool) -> Aabb {
    let h = if crouch { 1.5 } else { 1.8 };
    Aabb { lo: [feet[0] - 0.3, feet[1], feet[2] - 0.3], hi: [feet[0] + 0.3, feet[1] + h, feet[2] + 0.3] }
}

impl ClientSim {
    /// Цель удара под прицелом: моб (`aim_mob`) или другой игрок (`aim_player`) — ближайший на луче в 3 блоках и ближе
    /// блока; тогда блок под прицелом не выбран (ни рамки, ни ломания).
    pub(crate) fn aim_entities(&mut self, w: &mut WorldData) {
        self.aim_player = None;
        self.aim_mobs(w);
        let eye = self.ctl.player.eye(1.0);
        let dir = self.ctl.player.look();
        let far = [eye[0] + dir[0] * ENTITY_REACH, eye[1] + dir[1] * ENTITY_REACH, eye[2] + dir[2] * ENTITY_REACH];
        let mut best: Option<(u32, f64)> = None;
        for (&id, r) in &self.remotes {
            let (pos, _, _, flags) = r.at(r.t);
            if flags & PM_DEAD != 0 {
                continue;
            }
            let bx = body_box(pos, flags & PM_CROUCH != 0);
            let inside = (0..3).all(|k| bx.lo[k] <= eye[k] && eye[k] <= bx.hi[k]);
            let t = if inside { Some(0.0) } else { seg_box_t(eye, far, &bx) };
            if let Some(t) = t {
                let d = t * ENTITY_REACH;
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((id, d));
                }
            }
        }
        let Some((id, d)) = best else { return };
        // Ближе моба (если он тоже под прицелом) и ближе блока.
        if let Some(m) = self.aim_mob {
            let md = self.mobs.pick(eye, dir, ENTITY_REACH).filter(|(mid, _)| *mid == m).map_or(f64::MAX, |(_, md)| md);
            if md <= d {
                return;
            }
        }
        let block = self.ctl.target.map(|h| (0..3).map(|k| (h.point[k] - eye[k]).powi(2)).sum::<f64>().sqrt());
        if self.aim_mob.is_some() || block.is_none_or(|b| d < b) {
            self.aim_mob = None;
            self.aim_player = Some(id);
            self.ctl.target = None;
        }
    }

    /// ЛКМ по игроку под прицелом (doAttack → attackEntity): удар серверу и взмах. true — был игрок.
    pub(crate) fn attack_aimed_player(&mut self) -> bool {
        let Some(id) = self.aim_player else { return false };
        self.sync_sel();
        self.acts.push(Act::Attack { target: id });
        self.swing();
        true
    }
}
