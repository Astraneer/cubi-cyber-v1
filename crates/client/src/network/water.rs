//! Bounded receipt/ACK ownership for sanitized far-water halos. ACK means the
//! public image is retained; incomplete water topology may still hold GPU cover.
use super::{ClientMessage, Connection, NetworkError, PageId};
use net::water::{Request, WaterHalo};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

#[derive(Clone, Copy)]
struct Interest {
    request: Request,
    last_request: u64,
    received: bool,
}
#[derive(Default)]
pub(super) struct WaterState {
    interests: BTreeMap<PageId, Interest>,
    images: BTreeMap<PageId, Arc<WaterHalo>>,
    updates: VecDeque<PageId>,
}
fn same_binding(a: Request, b: Request) -> bool {
    a.page == b.page
        && a.geometry_source == b.geometry_source
        && a.geometry_view == b.geometry_view
        && a.column_source == b.column_source
}
impl Connection {
    pub fn request_water(&mut self, mut request: Request, now_ms: u64) -> Result<(), NetworkError> {
        request.validate().map_err(|_| NetworkError::Protocol)?;
        if self.welcome.is_none() {
            return Err(NetworkError::NotConnected);
        }
        if !self.water.interests.contains_key(&request.page)
            && self.water.interests.len() >= net::MAX_PUBLIC_GEOMETRY_PAGES
        {
            return Err(NetworkError::Capacity);
        }
        let same = self
            .water
            .interests
            .get(&request.page)
            .is_some_and(|old| same_binding(old.request, request));
        request.acknowledged = if same {
            self.water
                .images
                .get(&request.page)
                .map(|image| image.water_view)
        } else {
            None
        };
        self.queue(ClientMessage::RequestWater(request))?;
        if !same {
            self.water.images.remove(&request.page);
            self.water.updates.retain(|&page| page != request.page);
        }
        self.water.interests.insert(
            request.page,
            Interest {
                request,
                last_request: now_ms,
                received: request.acknowledged.is_some(),
            },
        );
        Ok(())
    }
    pub fn release_water(&mut self, page: PageId) -> Result<(), NetworkError> {
        self.queue(ClientMessage::ForgetWater(page))?;
        self.water.interests.remove(&page);
        self.water.images.remove(&page);
        self.water.updates.retain(|&key| key != page);
        Ok(())
    }
    pub fn water_image(&self, page: PageId) -> Option<&Arc<WaterHalo>> {
        self.water.images.get(&page)
    }
    pub fn take_water_update(&mut self) -> Option<Arc<WaterHalo>> {
        while let Some(page) = self.water.updates.pop_front() {
            if let Some(image) = self.water.images.get(&page) {
                return Some(Arc::clone(image));
            }
        }
        None
    }
    pub fn water_queue_depths(&self) -> (usize, usize, usize) {
        (
            self.water.interests.len(),
            self.water.images.len(),
            self.water.updates.len(),
        )
    }
    pub(super) fn receive_water(
        &mut self,
        halo: Box<WaterHalo>,
        now_ms: u64,
    ) -> Result<(), NetworkError> {
        halo.validate().map_err(|_| NetworkError::Protocol)?;
        let Some(interest) = self.water.interests.get(&halo.page).copied() else {
            return Ok(());
        };
        if !same_binding(
            interest.request,
            Request {
                page: halo.page,
                geometry_source: halo.geometry_source,
                geometry_view: halo.geometry_view,
                column_source: halo.column_source,
                acknowledged: None,
            },
        ) {
            return Ok(());
        }
        let old = self.water.images.get(&halo.page);
        if old.is_some_and(|old| old.water_view == halo.water_view && **old != *halo) {
            return Err(NetworkError::Protocol);
        }
        let newer = old.is_none_or(|old| halo.water_view > old.water_view);
        let acknowledged = old.map_or(halo.water_view, |old| old.water_view.max(halo.water_view));
        // Queue credit before mutating the bounded cache. Unknown-containing
        // images are ACKed too, allowing a repaired neighbour halo to advance.
        self.queue(ClientMessage::RequestWater(Request {
            acknowledged: Some(acknowledged),
            ..interest.request
        }))?;
        if newer {
            let page = halo.page;
            self.water.images.insert(page, Arc::from(halo));
            if !self.water.updates.contains(&page) {
                self.water.updates.push_back(page);
            }
        }
        if let Some(interest) = self.water.interests.get_mut(&interest.request.page) {
            interest.received = true;
            interest.last_request = now_ms;
        }
        Ok(())
    }
    pub(super) fn water_unavailable(&mut self, page: PageId, now_ms: u64) {
        if let Some(interest) = self.water.interests.get_mut(&page) {
            interest.received = false;
            interest.last_request = now_ms;
        }
    }
    pub(super) fn poll_water(&mut self, now_ms: u64) -> Result<(), NetworkError> {
        let mut retry = [None; 4];
        for (slot, interest) in retry.iter_mut().zip(
            self.water
                .interests
                .values()
                .filter(|i| !i.received && now_ms.saturating_sub(i.last_request) >= 1000),
        ) {
            *slot = Some(interest.request);
        }
        for request in retry.into_iter().flatten() {
            self.request_water(request, now_ms)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::{SpaceId, Version};
    fn packet(view: u64) -> Box<WaterHalo> {
        Box::new(WaterHalo {
            page: PageId {
                space: SpaceId(0),
                x: 4,
                y: 3,
                z: 2,
            },
            geometry_source: Version(7),
            geometry_view: 11,
            water_view: view,
            column_source: 13,
            cells: Box::new([0; 5832]),
            tints: [0xffff; 256],
        })
    }
    fn connection() -> Connection {
        let mut c = Connection::new(super::super::NetworkConfig::default()).unwrap();
        let h = packet(1);
        c.water.interests.insert(
            h.page,
            Interest {
                request: Request {
                    page: h.page,
                    geometry_source: h.geometry_source,
                    geometry_view: h.geometry_view,
                    column_source: h.column_source,
                    acknowledged: None,
                },
                last_request: 0,
                received: false,
            },
        );
        c
    }
    #[test]
    fn unknown_halo_receipt_releases_credit_but_conflicting_and_stale_images_never_replace_it() {
        let mut c = connection();
        c.receive_water(packet(3), 0).unwrap();
        assert_eq!(c.water_queue_depths(), (1, 1, 1));
        assert!(matches!(
            c.take_outgoing(256).as_slice(),
            [ClientMessage::RequestWater(Request {
                acknowledged: Some(3),
                ..
            })]
        ));
        assert!(
            c.take_water_update()
                .unwrap()
                .cells
                .iter()
                .all(|&cell| cell == 0)
        );
        c.receive_water(packet(2), 1).unwrap();
        assert_eq!(c.water_image(packet(1).page).unwrap().water_view, 3);
        assert!(c.take_water_update().is_none());
        let mut conflict = packet(3);
        conflict.cells[0] = 1;
        assert_eq!(c.receive_water(conflict, 2), Err(NetworkError::Protocol));
        let mut stale = packet(4);
        stale.geometry_view = 10;
        c.receive_water(stale, 3).unwrap();
        assert_eq!(c.water_image(packet(1).page).unwrap().water_view, 3);
    }
    #[test]
    fn full_outbox_refuses_cache_admission_and_duplicate_updates_are_coalesced() {
        let mut c = connection();
        for i in 0..super::super::MAX_OUTBOX {
            c.outbox.push_back(ClientMessage::Ping(i as u64));
        }
        assert_eq!(c.receive_water(packet(1), 0), Err(NetworkError::Capacity));
        assert_eq!(c.water_queue_depths(), (1, 0, 0));
        c.outbox.clear();
        for version in 1..=100 {
            c.receive_water(packet(version), version).unwrap();
        }
        assert_eq!(c.water_queue_depths(), (1, 1, 1));
        assert_eq!(c.outbox.len(), 1);
        assert_eq!(c.take_water_update().unwrap().water_view, 100);
    }
}
