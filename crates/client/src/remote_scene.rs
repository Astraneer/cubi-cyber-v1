//! Confirmed network images are distinct from local cache revisions and client
//! predictions. Remote scenes never generate a missing block page.
use super::*;
use core_types::{Cell, LocalIdx, PAGE_VOLUME};
use net::{PageSnapshot, PageUpdate};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmedPageVersion {
    pub view: u64,
    pub source: Version,
}
#[derive(Debug)]
pub struct AuthoritativeImport {
    pub applied: bool,
    pub change: Option<WorldChange>,
    pub cells_scanned: usize,
}
impl ClientScene {
    /// Welcome establishes public metadata before any authoritative page arrives.
    /// Reconnecting to another world uses a fresh scene and worker pool instead of
    /// changing the seed under already published meshes.
    pub fn configure_authority(
        &mut self,
        public_seed: u32,
        center: BlockPos,
    ) -> Result<(), SceneError> {
        if !self.config.authoritative_remote
            || self.world.version() != Version(0)
            || !self.metadata.is_empty()
        {
            return Err(SceneError::AuthorityMode);
        }
        let generator = terrain_gen::Generator::new(terrain_gen::TerrainSeed(public_seed));
        self.set_center(center)?;
        self.config.seed = terrain_gen::TerrainSeed(public_seed);
        self.remote_biomes = Some(generator);
        self.tints.clear();
        Ok(())
    }
    pub fn authoritative_version(&self, page: PageId) -> Option<ConfirmedPageVersion> {
        self.confirmed_versions.get(&page).copied()
    }
    fn check_remote_page(&self, page: PageId) -> Result<(), SceneError> {
        if !self.config.authoritative_remote {
            return Err(SceneError::AuthorityMode);
        }
        if !self.requests.contains_key(&page) {
            return Err(SceneError::UnrequestedPage(page));
        }
        Ok(())
    }
    fn prepare_remote_tint(&self, page: PageId) -> Result<Option<Box<[u16; 256]>>, SceneError> {
        if self.tints.contains_key(&page) {
            return Ok(None);
        }
        let generator = self
            .remote_biomes
            .as_ref()
            .ok_or(SceneError::AuthorityMode)?;
        let x0 = page
            .x
            .checked_mul(16)
            .ok_or(SceneError::InvalidAuthoritativeUpdate)?;
        let z0 = page
            .z
            .checked_mul(16)
            .ok_or(SceneError::InvalidAuthoritativeUpdate)?;
        let mut tint = Box::new([0; 256]);
        for z in 0..16 {
            for x in 0..16 {
                let x = x0
                    .checked_add(x)
                    .ok_or(SceneError::InvalidAuthoritativeUpdate)?;
                let z = z0
                    .checked_add(z)
                    .ok_or(SceneError::InvalidAuthoritativeUpdate)?;
                // A bounded public biome query, not terrain/ore generation. There
                // are exactly 256 queries on first import, zero on subsequent deltas.
                tint[((z - z0) * 16 + x - x0) as usize] = generator.column_info(x, z).tint;
            }
        }
        Ok(Some(tint))
    }
    pub fn import_authoritative_page(
        &mut self,
        update: &PageUpdate,
        tick: Tick,
    ) -> Result<AuthoritativeImport, SceneError> {
        let (page, view, source) = match update {
            PageUpdate::Snapshot(p) => (p.page, p.version, p.source_version),
            PageUpdate::Delta(p) => (p.page, p.version, p.source_version),
        };
        self.check_remote_page(page)?;
        if view == 0 {
            return Err(SceneError::InvalidAuthoritativeUpdate);
        }
        let prior = self.confirmed_versions.get(&page).copied();
        if prior.is_some_and(|old| old.view >= view) {
            return Ok(AuthoritativeImport {
                applied: false,
                change: None,
                cells_scanned: 0,
            });
        }
        if prior.is_some_and(|old| old.source > source) {
            return Err(SceneError::InvalidAuthoritativeUpdate);
        }
        if let PageUpdate::Delta(delta) = update {
            let have = prior.map(|p| p.view);
            if have != Some(delta.base) || self.world.page(page).is_none() {
                return Err(SceneError::MissingAuthoritativeBase {
                    page,
                    have,
                    expected: delta.base,
                });
            }
            if delta.version <= delta.base || delta.changes.len() > PAGE_VOLUME {
                return Err(SceneError::InvalidAuthoritativeUpdate);
            }
            let mut seen = [0_u64; 64];
            for &(index, _) in &delta.changes {
                if usize::from(index) >= PAGE_VOLUME {
                    return Err(SceneError::InvalidAuthoritativeUpdate);
                }
                let bit = 1_u64 << (index % 64);
                let word = &mut seen[usize::from(index) / 64];
                if *word & bit != 0 {
                    return Err(SceneError::InvalidAuthoritativeUpdate);
                }
                *word |= bit;
            }
        }
        let tint = self.prepare_remote_tint(page)?;
        let snapshot = self.world.snapshot(page);
        let mut tx = Transaction::new(Tick(tick.0.max(self.world.tick().0)), page.space);
        let mut changed = false;
        let cells_scanned = match update {
            PageUpdate::Snapshot(image) => {
                if let Some(old) = &snapshot {
                    for (index, &cell) in image.cells.iter().enumerate() {
                        let local = LocalIdx::new(index as u16).unwrap();
                        if old.cell(local) != cell {
                            tx.set_cell(remote_position(page, local)?, cell);
                            changed = true;
                        }
                    }
                } else {
                    tx.create_page(page, image.cells.to_vec());
                    changed = true;
                }
                PAGE_VOLUME
            }
            PageUpdate::Delta(delta) => {
                let old = snapshot.as_ref().expect("validated loaded delta base");
                for &(index, cell) in &delta.changes {
                    let local = LocalIdx::new(index).expect("validated cell index");
                    if old.cell(local) != cell {
                        tx.set_cell(remote_position(page, local)?, cell);
                        changed = true;
                    }
                }
                delta.changes.len()
            }
        };
        let change = if changed {
            Some(self.apply_internal(tx, false)?)
        } else {
            None
        };
        if let Some(tint) = tint {
            self.tints.insert(page, tint);
        }
        self.confirmed_versions
            .insert(page, ConfirmedPageVersion { view, source });
        if let Some(request) = self.requests.get_mut(&page) {
            request.state = RequestState::Loaded;
        }
        Ok(AuthoritativeImport {
            applied: true,
            change,
            cells_scanned,
        })
    }
    /// A confirmed replica image can repair a missed local delta without asking
    /// the authority to regenerate or resend unchanged terrain.
    pub fn import_replica_page(
        &mut self,
        page: &PageSnapshot,
        tick: Tick,
    ) -> Result<AuthoritativeImport, SceneError> {
        self.import_authoritative_page(&PageUpdate::Snapshot(page.clone()), tick)
    }
    /// Returns a guarded optimistic change, never a saved singleplayer override.
    /// The connection owns its action ID, authority reply and rollback policy.
    pub fn apply_remote_prediction(
        &mut self,
        position: BlockPos,
        expected: Cell,
        value: Cell,
        tick: Tick,
    ) -> Result<WorldChange, SceneError> {
        self.check_remote_page(position.page())?;
        if self.world.read(position) != world::CellRead::Known(expected) {
            return Err(SceneError::PredictionConflict);
        }
        let mut tx = Transaction::new(Tick(tick.0.max(self.world.tick().0)), position.space);
        tx.expect_page(position.page(), self.world.page_version(position.page()));
        tx.set_cell(position, value);
        self.apply_internal(tx, false)
    }
    pub fn remove_authoritative_page(
        &mut self,
        page: PageId,
        tick: Tick,
    ) -> Result<Option<WorldChange>, SceneError> {
        if !self.config.authoritative_remote {
            return Err(SceneError::AuthorityMode);
        }
        if self.world.page(page).is_none() {
            self.confirmed_versions.remove(&page);
            return Ok(None);
        }
        let mut tx = Transaction::new(Tick(tick.0.max(self.world.tick().0)), page.space);
        if let Some(snapshot) = self.world.snapshot(page) {
            for (_, id) in snapshot.component_links() {
                tx.remove_component(id, core_types::ChannelMask::CONTENTS);
            }
        }
        tx.remove_page(page);
        let change = self.apply_internal(tx, false)?;
        self.confirmed_versions.remove(&page);
        self.hidden_pages.insert(page);
        Ok(Some(change))
    }
}
fn remote_position(page: PageId, index: LocalIdx) -> Result<BlockPos, SceneError> {
    let (x, y, z) = index.xyz();
    let axis = |p: i32, v: u8| {
        p.checked_mul(16)
            .and_then(|p| p.checked_add(i32::from(v)))
            .ok_or(SceneError::InvalidAuthoritativeUpdate)
    };
    Ok(BlockPos {
        space: page.space,
        x: axis(page.x, x)?,
        y: axis(page.y, y)?,
        z: axis(page.z, z)?,
    })
}
