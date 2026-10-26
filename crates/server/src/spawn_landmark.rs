//! The public square is announced only after its Welcome crosses the durable
//! frontier. Its location is independent of saved player/login coordinates.
use super::*;

impl Server {
    pub(super) fn append_spawn_landmark(
        &self,
        connection: u64,
        session: u64,
        outgoing: &mut Vec<Outgoing>,
    ) {
        let Some([x, y, z]) = self.showcase_center() else {
            return;
        };
        outgoing.push(Outgoing::reliable(
            connection,
            ServerMessage::SpawnLandmark(net::spawn::Landmark {
                session,
                center: BlockPos {
                    space: SpaceId(0),
                    x,
                    y,
                    z,
                },
            }),
        ));
    }
}
