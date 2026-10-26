//! Per-page public pipe-item history. Only committed owner rows are read;
//! confirmed revisions remain independent from the most recently sent image.
use super::*;
use net::machine_packets::{self as wire, Frame, Pose, Request};
use std::collections::VecDeque;
#[derive(Clone)]
struct Image {
    revision: u64,
    tick: Tick,
    rows: BTreeMap<u64, Pose>,
}
pub struct PacketHistory {
    page: PageId,
    view_version: Version,
    confirmed: Option<u64>,
    next_revision: u64,
    images: VecDeque<Image>,
    maximum: usize,
    versions: usize,
    force_full: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PacketWork {
    pub indexed_rows: u64,
    pub upserts: u64,
    pub removes: u64,
    pub full: bool,
}
impl PacketHistory {
    pub fn new(
        page: PageId,
        view_version: Version,
        maximum: usize,
        versions: usize,
    ) -> Result<Self, MachineError> {
        if maximum == 0 || maximum > wire::MAX_PACKETS || !(1..=4).contains(&versions) {
            return Err(MachineError::Bounds);
        }
        Ok(Self {
            page,
            view_version,
            confirmed: None,
            next_revision: 1,
            images: VecDeque::with_capacity(versions),
            maximum,
            versions,
            force_full: false,
        })
    }
    pub fn confirmed(&self) -> Option<u64> {
        self.confirmed
    }
    pub fn set_view_version(&mut self, version: Version) {
        if self.view_version != version {
            self.view_version = version;
            self.images.clear();
            self.confirmed = None;
            self.force_full = true;
        }
    }
    pub fn request(&mut self, request: Request) -> bool {
        if request.page() != self.page || request.view_version() != self.view_version {
            return false;
        }
        match request {
            Request::Ack { revision, .. } => {
                if self.confirmed.is_some_and(|old| revision < old)
                    || !self.images.iter().any(|image| image.revision == revision)
                {
                    return false;
                }
                self.confirmed = Some(revision);
                true
            }
            Request::Full { .. } => {
                self.force_full = true;
                true
            }
        }
    }
    pub fn plan(
        &mut self,
        runtime: &MachineRuntime,
        tick: Tick,
    ) -> Result<(Option<Frame>, PacketWork), MachineError> {
        if self.images.back().is_some_and(|image| tick < image.tick) {
            return Err(MachineError::Stale);
        }
        let mut rows = BTreeMap::new();
        let mut work = PacketWork::default();
        for packet in runtime.packets_in_page(self.page).take(self.maximum + 1) {
            work.indexed_rows += 1;
            if rows.len() == self.maximum {
                return Err(MachineError::Bounds);
            }
            let at = packet.at().ok_or(MachineError::Encoding)?;
            let entry = transport::direction(packet.from, at).ok_or(MachineError::Encoding)? as u8;
            let exit = packet
                .path
                .get(usize::from(packet.step) + 1)
                .and_then(|&next| transport::direction(at, next))
                .map_or(wire::NO_EXIT, |face| face as u8);
            let (epoch, active) = runtime.packet_epoch(packet.id, tick)?;
            let clock = runtime
                .page_clock(at.page())
                .ok_or(MachineError::Encoding)?;
            let active = active && packet.last_tick == clock.local_now(tick)?;
            let pose = Pose {
                id: packet.id,
                index: at.local_index(),
                item: packet.stack.item(),
                count: packet.stack.count(),
                color: packet.color,
                entry,
                exit,
                progress: packet.progress,
                flags: if active { wire::ACTIVE } else { 0 },
                epoch,
            };
            pose.validate().map_err(|_| MachineError::Encoding)?;
            rows.insert(pose.id, pose);
        }
        if !self.force_full
            && self
                .images
                .back()
                .is_some_and(|old| same_image(&old.rows, &rows, tick))
        {
            return Ok((None, work));
        }
        let base = if self.force_full {
            None
        } else {
            self.confirmed
                .and_then(|revision| self.images.iter().find(|i| i.revision == revision))
        };
        let upserts: Vec<_> = rows
            .iter()
            .filter_map(|(&id, &pose)| {
                base.is_none_or(|b| {
                    b.rows
                        .get(&id)
                        .is_none_or(|&old| !old.same_motion(pose, tick))
                })
                .then_some(pose)
            })
            .collect();
        let removes: Vec<_> = base
            .into_iter()
            .flat_map(|b| b.rows.keys())
            .filter(|id| !rows.contains_key(id))
            .copied()
            .collect();
        let revision = self.next_revision;
        let next = revision.checked_add(1).ok_or(MachineError::Overflow)?;
        let frame = Frame {
            page: self.page,
            view_version: self.view_version,
            revision,
            base: base.map(|b| b.revision),
            tick,
            upserts,
            removes,
        };
        frame.validate().map_err(|_| MachineError::Encoding)?;
        work.upserts = frame.upserts.len() as u64;
        work.removes = frame.removes.len() as u64;
        work.full = frame.base.is_none();
        if self.images.len() == self.versions {
            self.images.pop_front();
        }
        self.images.push_back(Image {
            revision,
            tick,
            rows,
        });
        self.next_revision = next;
        self.force_full = false;
        Ok((Some(frame), work))
    }
}
fn same_image(a: &BTreeMap<u64, Pose>, b: &BTreeMap<u64, Pose>, tick: Tick) -> bool {
    a.len() == b.len()
        && a.iter().all(|(id, &pose)| {
            b.get(id)
                .is_some_and(|&other| pose.same_motion(other, tick))
        })
}
