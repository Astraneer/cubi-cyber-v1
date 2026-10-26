//! Independent confirmed LIGHT ownership. Updates never edit cells or touch mesh
//! inputs; a packet is meaningful only for its exact confirmed public cell view.
use super::*;
use core_types::{LocalIdx, PAGE_VOLUME};
use net::light::{Delta, Header, Snapshot, Update};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmedLightVersion {
    pub cell_view: u64,
    pub view: u64,
    pub source: Version,
}
#[derive(Debug)]
pub struct AuthoritativeLightImport {
    pub applied: bool,
    pub change: Option<WorldChange>,
    pub samples_scanned: usize,
}
enum Samples<'a> {
    Full(&'a [u32; PAGE_VOLUME]),
    Delta {
        base: u64,
        changes: &'a [(u16, u32)],
    },
}
impl ClientScene {
    /// Public retained header for diagnostics; this does not certify the current
    /// cells. Rendering must continue to use `authoritative_light_version`.
    pub fn retained_authoritative_light_version(
        &self,
        page: PageId,
    ) -> Option<ConfirmedLightVersion> {
        self.confirmed_lights.get(&page).copied()
    }
    /// None also means that a newer public cell image invalidated the binding;
    /// callers must not label an old volume as confirmed for a newer page.
    pub fn authoritative_light_version(&self, page: PageId) -> Option<ConfirmedLightVersion> {
        let light = self.confirmed_lights.get(&page).copied()?;
        (self.authoritative_version(page)?.view == light.cell_view).then_some(light)
    }
    pub fn read_authoritative_light(&self, position: BlockPos) -> Option<world::LightSample> {
        self.authoritative_light_version(position.page())?;
        self.world
            .page(position.page())
            .map(|p| p.light(position.local_index()))
    }
    pub fn import_authoritative_light(
        &mut self,
        update: &Update,
        tick: Tick,
    ) -> Result<AuthoritativeLightImport, SceneError> {
        update.validate().map_err(SceneError::Light)?;
        match update {
            Update::Snapshot(snapshot) => self.import_replica_light(snapshot, tick),
            Update::Delta(Delta {
                header,
                base,
                changes,
            }) => self.import_light_samples(
                *header,
                Samples::Delta {
                    base: *base,
                    changes,
                },
                tick,
            ),
        }
    }
    /// No clone of the16KiB confirmed image is needed for AOI rehydration.
    pub fn import_replica_light(
        &mut self,
        snapshot: &Snapshot,
        tick: Tick,
    ) -> Result<AuthoritativeLightImport, SceneError> {
        // Validate borrowed headers without copying the confirmed image.
        if snapshot.header.version == 0 || snapshot.header.cell_view == 0 {
            return Err(SceneError::Light(net::light::Error::InvalidVersion));
        }
        self.import_light_samples(snapshot.header, Samples::Full(&snapshot.values), tick)
    }
    fn import_light_samples(
        &mut self,
        header: Header,
        samples: Samples<'_>,
        tick: Tick,
    ) -> Result<AuthoritativeLightImport, SceneError> {
        self.check_remote_page(header.page)?;
        let empty = || AuthoritativeLightImport {
            applied: false,
            change: None,
            samples_scanned: 0,
        };
        let cell_view = self.authoritative_version(header.page).map(|v| v.view);
        if cell_view.is_some_and(|view| header.cell_view < view) {
            return Ok(empty());
        }
        if cell_view != Some(header.cell_view) || self.world.page(header.page).is_none() {
            return Err(SceneError::Light(net::light::Error::MissingCells {
                page: header.page,
                have: cell_view,
                expected: header.cell_view,
            }));
        }
        let prior = self.confirmed_lights.get(&header.page).copied();
        if prior.is_some_and(|old| old.view >= header.version) {
            return Ok(empty());
        }
        if prior.is_some_and(|old| old.source > header.source || old.cell_view > header.cell_view) {
            return Err(SceneError::Light(net::light::Error::InvalidVersion));
        }
        if let Samples::Delta { base, .. } = &samples {
            let have = prior.map(|v| v.view);
            if have != Some(*base) {
                return Err(SceneError::Light(net::light::Error::MissingBase {
                    page: header.page,
                    have,
                    expected: *base,
                }));
            }
        }
        let mut tx = Transaction::new(Tick(tick.0.max(self.world.tick().0)), header.page.space);
        tx.expect_page(header.page, self.world.page_version(header.page));
        let page = self
            .world
            .page(header.page)
            .expect("checked confirmed page");
        let mut changed = false;
        let mut scanned = 0;
        let mut stage = |index: u16, value: u32| -> Result<(), SceneError> {
            let local = LocalIdx::new(index).ok_or(SceneError::InvalidAuthoritativeUpdate)?;
            let (sky, block, rgb) = net::light::unpack(value);
            let next = world::LightSample { sky, block, rgb };
            scanned += 1;
            if page.light(local) != next {
                let (x, y, z) = local.xyz();
                let axis = |base: i32, local: u8| {
                    base.checked_mul(16)
                        .and_then(|v| v.checked_add(i32::from(local)))
                        .ok_or(SceneError::InvalidAuthoritativeUpdate)
                };
                tx.set_light(
                    BlockPos {
                        space: header.page.space,
                        x: axis(header.page.x, x)?,
                        y: axis(header.page.y, y)?,
                        z: axis(header.page.z, z)?,
                    },
                    next,
                );
                changed = true;
            }
            Ok(())
        };
        match samples {
            Samples::Full(values) => {
                for (index, value) in values.iter().enumerate() {
                    stage(index as u16, *value)?;
                }
            }
            Samples::Delta { changes, .. } => {
                for (index, value) in changes {
                    stage(*index, *value)?;
                }
            }
        }
        let change = if changed {
            Some(self.apply_internal(tx, false)?)
        } else {
            None
        };
        self.confirmed_lights.insert(
            header.page,
            ConfirmedLightVersion {
                cell_view: header.cell_view,
                view: header.version,
                source: header.source,
            },
        );
        Ok(AuthoritativeLightImport {
            applied: true,
            change,
            samples_scanned: scanned,
        })
    }
}
