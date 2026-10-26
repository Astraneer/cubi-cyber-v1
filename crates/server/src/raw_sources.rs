//! Raw public cell provenance is independent of derived LIGHT and components.
//! This resident-only index is rebuilt after recovery. It never stores cells or
//! private payloads and advances only after the owner's durable boundary.
use core_types::{PageId, Version};
use std::collections::BTreeMap;
use world::{World, WorldChange};

pub(super) struct RawSources {
    pages: BTreeMap<PageId, Version>,
}
impl RawSources {
    pub(super) fn new(world: &World) -> Self {
        Self {
            // A recovered overall stamp is conservative and valid for these
            // exact cells. Subsequent LIGHT-only revisions cannot advance it.
            pages: world
                .pages()
                .map(|(id, page)| (id, page.version()))
                .collect(),
        }
    }
    pub(super) fn source(&self, page: PageId) -> Option<Version> {
        self.pages.get(&page).copied()
    }
    pub(super) fn observe(&mut self, changes: &[WorldChange]) {
        for change in changes {
            for page in &change.pages {
                if page.removed {
                    self.pages.remove(&page.page);
                } else if page.created
                    // World commits and decoded durable records keep cells in
                    // (PageId, LocalIdx) order. No scan of unchanged pages or
                    // 4096-cell payloads is needed, even for ChannelMask::NONE.
                    || change.cells.binary_search_by_key(&page.page, |c| c.page).is_ok()
                {
                    self.pages.insert(page.page, page.after);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::{BlockPos, Cell, ChannelMask, SpaceId, Tick};
    use net::{PageUpdate, ViewHistory};
    use world::{LightSample, Transaction};

    fn position() -> BlockPos {
        BlockPos {
            space: SpaceId(0),
            x: 2,
            y: 2,
            z: 2,
        }
    }
    fn fixture() -> (World, RawSources) {
        let mut world = World::new(rules::properties, 32);
        let mut tx = Transaction::new(Tick(1), SpaceId(0));
        tx.create_page(position().page(), vec![Cell::AIR; 4096]);
        world.commit(tx).unwrap();
        let sources = RawSources::new(&world);
        (world, sources)
    }
    #[test]
    fn derived_light_does_not_rebind_cells_but_channel_free_cell_state_does() {
        let (mut world, mut sources) = fixture();
        let page = position().page();
        let initial = sources.source(page).unwrap();
        let mut history = ViewHistory::new(1, 4);
        let image = history
            .build(page, initial, world.page(page).unwrap().cells(), false)
            .unwrap()
            .unwrap();
        let PageUpdate::Snapshot(image) = image else {
            panic!("initial image");
        };
        assert!(history.acknowledge(page, image.version));

        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.set_light(
            position(),
            LightSample {
                sky: 15,
                block: 0,
                rgb: [0; 3],
            },
        );
        let light = world.commit(tx).unwrap();
        assert!(light.cells.is_empty());
        assert!(world.page_version(page) > initial);
        sources.observe(&[light]);
        assert_eq!(sources.source(page), Some(initial));
        assert!(
            history
                .build(
                    page,
                    sources.source(page).unwrap(),
                    world.page(page).unwrap().cells(),
                    false
                )
                .unwrap()
                .is_none()
        );

        // Identical derived properties do not imply identical raw cell bits.
        // This state mutation must still get a new source and delta.
        let mut tx = Transaction::new(Tick(3), SpaceId(0));
        tx.set_cell(position(), Cell::from_parts(rules::ids::AIR, 9));
        let raw = world.commit(tx).unwrap();
        assert_eq!(raw.channels, ChannelMask::NONE);
        assert_eq!(raw.cells.len(), 1);
        let after = world.page_version(page);
        sources.observe(&[raw]);
        assert_eq!(sources.source(page), Some(after));
        let delta = history
            .build(page, after, world.page(page).unwrap().cells(), false)
            .unwrap()
            .unwrap();
        let PageUpdate::Delta(delta) = delta else {
            panic!("single cell delta");
        };
        assert_eq!(delta.source_version, after);
        assert_eq!(
            delta.changes,
            [(
                position().local_index().get(),
                Cell::from_parts(rules::ids::AIR, 9)
            )]
        );
    }
    #[test]
    fn removal_recreation_and_recovery_keep_conservative_monotone_sources() {
        let (mut world, mut sources) = fixture();
        let page = position().page();
        let before = sources.source(page).unwrap();
        let mut tx = Transaction::new(Tick(2), SpaceId(0));
        tx.remove_page(page);
        sources.observe(&[world.commit(tx).unwrap()]);
        assert_eq!(sources.source(page), None);
        let mut tx = Transaction::new(Tick(3), SpaceId(0));
        tx.create_page(page, vec![Cell::AIR; 4096]);
        sources.observe(&[world.commit(tx).unwrap()]);
        let recreated = sources.source(page).unwrap();
        assert!(recreated > before);
        let mut tx = Transaction::new(Tick(4), SpaceId(0));
        tx.set_light(
            position(),
            LightSample {
                sky: 9,
                block: 0,
                rgb: [0; 3],
            },
        );
        sources.observe(&[world.commit(tx).unwrap()]);
        assert_eq!(sources.source(page), Some(recreated));
        let restored = RawSources::new(&world);
        assert_eq!(restored.source(page), Some(world.page_version(page)));
        assert!(restored.source(page).unwrap() > recreated);
    }
}
