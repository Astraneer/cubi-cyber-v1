//! Public pose selection; the server independently checks actual body and LOS.
use super::*;
use core_types::SpaceId;
fn ray_box(
    eye: [f64; 3],
    direction: [f64; 3],
    lo: [f64; 3],
    hi: [f64; 3],
    reach: f64,
) -> Option<f64> {
    let (mut near, mut far) = (0f64, reach);
    for axis in 0..3 {
        if direction[axis].abs() < 1e-15 {
            if eye[axis] < lo[axis] || eye[axis] > hi[axis] {
                return None;
            }
            continue;
        }
        let mut a = (lo[axis] - eye[axis]) / direction[axis];
        let mut b = (hi[axis] - eye[axis]) / direction[axis];
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        near = near.max(a);
        far = far.min(b);
        if near > far {
            return None;
        }
    }
    Some(near)
}
pub(super) fn pick(
    connection: &Connection,
    space: SpaceId,
    eye: [f64; 3],
    direction: [f64; 3],
    reach: f64,
) -> Option<u64> {
    let mut best: Option<(f64, u64)> = None;
    let mut consider = |id, position: [f64; 3], width: f64, height: f64| {
        let radius = width / 2.;
        let lo = [position[0] - radius, position[1], position[2] - radius];
        let hi = [
            position[0] + radius,
            position[1] + height,
            position[2] + radius,
        ];
        if let Some(distance) = ray_box(eye, direction, lo, hi, reach)
            && best.is_none_or(|(old, old_id)| distance < old || (distance == old && id < old_id))
        {
            best = Some((distance, id));
        }
    };
    for entity in connection.entity_replica().poses() {
        if entity.space != space || entity.kind > 4 || entity.flags & 4 != 0 {
            continue;
        }
        let definition = rules::entities::MobKind::from_byte(entity.kind)
            .expect("validated public kind")
            .definition();
        consider(
            entity.id,
            entity.position,
            definition.width * f64::from(entity.scale),
            definition.height * f64::from(entity.scale),
        );
    }
    let own = connection.welcome().map(|w| w.identity);
    for player in connection.remote_players() {
        if Some(player.identity) == own
            || player.space != space
            || player.mode == 3
            || player.health <= 0.
        {
            continue;
        }
        consider(
            net::player_entity_id(player.identity),
            player.position,
            0.6,
            1.8,
        );
    }
    let (distance, id) = best?;
    match sim::raycast(
        &RemoteQuery { connection, space },
        eye,
        direction,
        distance + 1e-7,
    ) {
        RayResult::Miss => Some(id),
        RayResult::Hit(hit) if hit.distance + 1e-7 >= distance => Some(id),
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use core_types::{PageId, Version};
    use net::{
        PageSnapshot, PageUpdate, ServerMessage,
        entities::{EntityFrame, EntityPose},
    };
    fn setup(blocked: bool) -> Connection {
        let mut c = Connection::new(NetworkConfig::default()).unwrap();
        let page = PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        };
        c.request_page(page, 0).unwrap();
        let mut cells = Box::new([Cell::AIR; 4096]);
        if blocked {
            cells[8 + 16 * 10 + 256] = Cell::from_parts(rules::ids::STONE, 0);
        }
        c.receive(
            ServerMessage::Page(PageUpdate::Snapshot(PageSnapshot {
                page,
                version: 1,
                source_version: Version(1),
                cells,
            })),
            1,
        )
        .unwrap();
        let pose = EntityPose {
            id: 9,
            space: SpaceId(0),
            position: [8.5, 1., 11.5],
            body_yaw: 0.,
            head_yaw: 0.,
            pitch: 0.,
            limb_phase: 0.,
            limb_amount: 0.,
            death_roll: 0.,
            scale: 1.,
            extra: 0.,
            kind: 0,
            flags: 0,
            color: 0,
            count: 0,
            damage: 0,
        };
        c.receive(
            ServerMessage::Entities(Box::new(EntityFrame {
                revision: 1,
                base: None,
                tick: Tick(1),
                upserts: vec![pose],
                removes: Vec::new(),
            })),
            1,
        )
        .unwrap();
        c
    }
    #[test]
    fn target_uses_public_body_and_never_selects_through_wall_or_unknown() {
        let mut c = setup(false);
        let eye = [8.5, 1.5, 8.5];
        let direction = [0., 0., 1.];
        assert_eq!(pick(&c, SpaceId(0), eye, direction, 6.), Some(9));
        assert_eq!(pick(&setup(true), SpaceId(0), eye, direction, 6.), None);
        let page = PageId {
            space: SpaceId(0),
            x: 0,
            y: 0,
            z: 0,
        };
        c.receive(ServerMessage::PageUnavailable(page), 2).unwrap();
        assert_eq!(pick(&c, SpaceId(0), eye, direction, 6.), None);
        assert_eq!(pick(&setup(false), SpaceId(1), eye, direction, 6.), None);
    }
}
