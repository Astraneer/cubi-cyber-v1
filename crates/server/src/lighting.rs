//! Authoritative CPU light adapter. Returned derived records belong to the same
//! durable server phase as gameplay; no renderer or generation call is made here.
#[cfg(test)]
mod continuation_tests;
mod durable;
use core_types::{BlockPos, PageId, SpaceId, Tick, Version};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};
use world::{
    CursorError, CursorId, World, WorldChange,
    lighting::{
        LIGHT_INPUT_CHANNELS, LightBudget, LightIngestBudget, LightPriority, LightProgress,
        LightingEngine, LightingError, LightingLimits, SkyAccess,
    },
};

/// Immutable authority statement: the canonical generator has no blocks above
/// profile_top, and a complete persisted edit/opaque index supplied every blocker
/// above it, including unloaded pages. Missing/legacy indices MUST set ready=false.
/// This is not a terrain height estimate and must never come from client data.
#[derive(Clone, Debug)]
pub struct GeneratedSkyProof {
    space: SpaceId,
    profile_top: i32,
    source: u64,
    ready: bool,
    blockers: BTreeMap<(i32, i32), i32>,
}
impl GeneratedSkyProof {
    pub fn from_authority(
        space: SpaceId,
        profile_top: i32,
        source: u64,
        ready: bool,
        opaque_above_profile: impl IntoIterator<Item = BlockPos>,
        max_columns: usize,
    ) -> Result<Self, ServerLightingError> {
        if source == 0 || max_columns == 0 {
            return Err(ServerLightingError::InvalidLimits);
        }
        let mut proof = Self {
            space,
            profile_top,
            source,
            ready,
            blockers: BTreeMap::new(),
        };
        if ready {
            for p in opaque_above_profile {
                if p.space != space || p.y <= profile_top {
                    continue;
                }
                let key = (p.x, p.z);
                if !proof.blockers.contains_key(&key) && proof.blockers.len() >= max_columns {
                    return Err(ServerLightingError::Capacity);
                }
                proof
                    .blockers
                    .entry(key)
                    .and_modify(|height| *height = (*height).max(p.y))
                    .or_insert(p.y);
            }
        }
        Ok(proof)
    }
    pub fn is_ready(&self) -> bool {
        self.ready
    }
    pub fn source(&self) -> u64 {
        self.source
    }
    pub fn blocker_columns(&self) -> usize {
        self.blockers.len()
    }
}
impl SkyAccess for GeneratedSkyProof {
    fn incoming_from_unknown_above(&self, position: BlockPos) -> Option<u8> {
        if !self.ready || position.space != self.space {
            return None;
        }
        let ceiling = self
            .blockers
            .get(&(position.x, position.z))
            .copied()
            .unwrap_or(self.profile_top);
        (position.y >= ceiling).then_some(15)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ServerLightingConfig {
    pub engine: LightingLimits,
    /// Only immutable page keys are collected during attach, no cell scan.
    pub max_bootstrap_pages: usize,
    pub bootstrap_pages_per_phase: usize,
    pub ingest: LightIngestBudget,
    pub step: LightBudget,
}
impl Default for ServerLightingConfig {
    fn default() -> Self {
        Self {
            engine: LightingLimits::default(),
            max_bootstrap_pages: 4096,
            bootstrap_pages_per_phase: 1,
            ingest: LightIngestBudget::default(),
            step: LightBudget::default(),
        }
    }
}
#[derive(Debug)]
pub enum ServerLightingError {
    InvalidLimits,
    Capacity,
    AwaitingDurability,
    InvalidDurableToken,
    CaptureInvariant,
    Cursor(CursorError),
    Engine(LightingError),
}
impl std::fmt::Display for ServerLightingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "server lighting: {self:?}")
    }
}
impl std::error::Error for ServerLightingError {}
impl ServerLightingError {
    /// Fixed diagnostic classes contain no cells, positions or private state.
    pub fn diagnostic(&self) -> &'static str {
        match self {
            Self::InvalidLimits => "lighting limits",
            Self::Capacity => "lighting capacity",
            Self::AwaitingDurability => "lighting awaiting durability",
            Self::InvalidDurableToken => "lighting durable token",
            Self::CaptureInvariant => "lighting capture invariant",
            Self::Cursor(CursorError::Lagged { .. }) => "lighting capture cursor lagged",
            Self::Cursor(_) => "lighting capture cursor",
            Self::Engine(LightingError::Cursor(CursorError::Lagged { .. })) => {
                "lighting input cursor lagged"
            }
            Self::Engine(LightingError::Cursor(_)) => "lighting input cursor",
            Self::Engine(LightingError::InvalidLimits) => "lighting engine limits",
            Self::Engine(LightingError::TickWentBackwards) => "lighting tick order",
            Self::Engine(LightingError::CoordinateOverflow(_)) => "lighting coordinate overflow",
            Self::Engine(LightingError::Commit(_)) => "lighting commit",
            Self::Engine(LightingError::Persistence(_)) => "lighting persistence",
        }
    }
}
impl From<CursorError> for ServerLightingError {
    fn from(error: CursorError) -> Self {
        Self::Cursor(error)
    }
}
impl From<LightingError> for ServerLightingError {
    fn from(error: LightingError) -> Self {
        Self::Engine(error)
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ServerLightWork {
    pub bootstrap_page_probes: u64,
    pub bootstrap_pages_queued: u64,
    pub sky_pages_queued: u64,
    pub capture_recoveries: u64,
    pub input_recoveries: u64,
    pub recovery_page_probes: u64,
    pub foreground_cell_reads: u64,
}
/// The token is private and bound to this owner's exact captured journal range.
/// Append changes to the owner's pending WAL group, fsync, then acknowledge.
pub struct LightingPhase {
    pub changes: Vec<Arc<WorldChange>>,
    pub ingest: LightProgress,
    pub progress: LightProgress,
    pub bootstrap_pending: usize,
    pub work: ServerLightWork,
    durable_token: Option<(u64, u64)>,
}
pub struct ServerLighting {
    config: ServerLightingConfig,
    engine: LightingEngine,
    capture: CursorId,
    bootstrap: VecDeque<PageId>,
    recovery_sources: BTreeMap<PageId, Version>,
    awaiting: Option<(u64, u64)>,
    nonce: u64,
    work: ServerLightWork,
    durable_revision: Option<u64>,
    bootstrap_removed: Vec<PageId>,
}
impl ServerLighting {
    pub fn attach(
        world: &mut World,
        config: ServerLightingConfig,
    ) -> Result<Self, ServerLightingError> {
        if config.max_bootstrap_pages == 0
            || config.bootstrap_pages_per_phase == 0
            || world.loaded_pages() > config.max_bootstrap_pages
        {
            return Err(ServerLightingError::InvalidLimits);
        }
        let engine = LightingEngine::attach(world, config.engine)?;
        let capture = world.register_cursor();
        let bootstrap = world.pages().map(|(page, _)| page).collect();
        let recovery_sources = world
            .pages()
            .map(|(id, page)| (id, page.channel_version(LIGHT_INPUT_CHANNELS)))
            .collect();
        Ok(Self {
            config,
            engine,
            capture,
            bootstrap,
            recovery_sources,
            awaiting: None,
            nonce: 0,
            work: ServerLightWork::default(),
            durable_revision: None,
            bootstrap_removed: Vec::new(),
        })
    }
    /// Supply cached loaded page keys affected by a new immutable sky proof.
    /// The caller owns column→loaded-page metadata; this never scans cold World.
    /// A blocked page is returned as false and must remain pending at the caller.
    pub fn request_sky_page(&mut self, page: PageId, tick: Tick) -> bool {
        let accepted = self
            .engine
            .queue_sky_boundary_page(page, tick, LightPriority::Interactive)
            .is_ok();
        self.work.sky_pages_queued += u64::from(accepted);
        accepted
    }
    pub fn pending_bootstrap_pages(&self) -> usize {
        self.bootstrap.len()
    }
    pub fn is_idle(&self) -> bool {
        self.bootstrap.is_empty() && self.awaiting.is_none() && self.engine.is_idle()
    }
    /// Single owner-thread phase. This mutates only derived LIGHT through World.
    /// A WAL failure after this call is fail-stop; do not publish or acknowledge.
    pub fn step(
        &mut self,
        world: &mut World,
        tick: Tick,
        sky: &impl SkyAccess,
    ) -> Result<LightingPhase, ServerLightingError> {
        if self.awaiting.is_some() {
            return Err(ServerLightingError::AwaitingDurability);
        }
        let boundary = world.journal_next();
        match world.acknowledge_cursor(self.capture, boundary) {
            Ok(()) => {}
            Err(CursorError::Lagged { .. }) => {
                // No derived group is outstanding. Earlier records are external
                // transactions already retained by the canonical server owner.
                world.recover_cursor(self.capture, boundary)?;
                self.work.capture_recoveries += 1;
            }
            Err(error) => return Err(error.into()),
        }
        // Derived LIGHT can evict older input history while bounded page seeds
        // drain. Recover from retained current source metadata, never by seeking
        // past an opacity/emission change without a complete reseed obligation.
        if !self.engine.has_durable_inputs()
            && matches!(
                world.read_cursor(self.engine.cursor(), 0),
                Err(CursorError::Lagged { .. })
            )
        {
            self.recover_inputs(world, boundary)?;
        }
        for _ in 0..self.config.bootstrap_pages_per_phase {
            let Some(page) = self.bootstrap.front().copied() else {
                break;
            };
            self.work.bootstrap_page_probes += 1;
            if self
                .engine
                .queue_page(page, tick, LightPriority::Background)
                .is_err()
            {
                break;
            }
            self.bootstrap.pop_front();
            if self.durable_revision.is_some() {
                self.bootstrap_removed.push(page);
            }
            self.work.bootstrap_pages_queued += 1;
        }
        let ingest = self.engine.ingest(world, tick, self.config.ingest)?;
        let progress = self.engine.step(world, tick, self.config.step, sky)?;
        let batch = world.read_cursor(self.capture, 2)?;
        if batch.changes.len() != progress.work.transactions as usize
            || batch.next != world.journal_next()
            || batch.changes.iter().any(|change| {
                change.lights.is_empty()
                    || !change.cells.is_empty()
                    || !change.components.is_empty()
                    || !change.events.is_empty()
                    || change.transform.is_some()
            })
        {
            return Err(ServerLightingError::CaptureInvariant);
        }
        let durable_token = if batch.changes.is_empty() {
            None
        } else {
            self.nonce = self
                .nonce
                .checked_add(1)
                .ok_or(ServerLightingError::Capacity)?;
            let token = (self.nonce, batch.next);
            self.awaiting = Some(token);
            Some(token)
        };
        Ok(LightingPhase {
            changes: batch.changes,
            ingest,
            progress,
            bootstrap_pending: self.bootstrap.len(),
            work: std::mem::take(&mut self.work),
            durable_token,
        })
    }
    fn recover_inputs(
        &mut self,
        world: &mut World,
        boundary: u64,
    ) -> Result<(), ServerLightingError> {
        if world.loaded_pages() > self.config.max_bootstrap_pages {
            return Err(ServerLightingError::Capacity);
        }
        let mut sources = BTreeMap::new();
        for (id, page) in world.pages() {
            self.work.recovery_page_probes += 1;
            sources.insert(id, page.channel_version(LIGHT_INPUT_CHANNELS));
        }
        // Preserve order and progress for pending current pages. Repeated lag
        // caused only by LIGHT/CONTENTS must not restart every page's seed.
        self.bootstrap.retain(|page| {
            self.work.recovery_page_probes += 1;
            sources.contains_key(page)
        });
        let mut pending: BTreeSet<_> = self.bootstrap.iter().copied().collect();
        for (&page, version) in &sources {
            self.work.recovery_page_probes += 1;
            if self.recovery_sources.get(&page) != Some(version) && pending.insert(page) {
                self.bootstrap.push_back(page);
            }
        }
        // A removed emitter/occluder can leave light in unchanged neighbours.
        // Retain current neighbours rather than an unbounded list of tombstones.
        for page in self.recovery_sources.keys() {
            self.work.recovery_page_probes += 1;
            if sources.contains_key(page) {
                continue;
            }
            for [dx, dy, dz] in [
                [-1, 0, 0],
                [1, 0, 0],
                [0, -1, 0],
                [0, 1, 0],
                [0, 0, -1],
                [0, 0, 1],
            ] {
                self.work.recovery_page_probes += 1;
                let (Some(x), Some(y), Some(z)) = (
                    page.x.checked_add(dx),
                    page.y.checked_add(dy),
                    page.z.checked_add(dz),
                ) else {
                    continue;
                };
                let neighbour = PageId {
                    space: page.space,
                    x,
                    y,
                    z,
                };
                if sources.contains_key(&neighbour) && pending.insert(neighbour) {
                    self.bootstrap.push_back(neighbour);
                }
            }
        }
        debug_assert!(self.bootstrap.len() <= self.config.max_bootstrap_pages);
        self.recovery_sources = sources;
        self.engine.recover_cursor(world, boundary)?;
        self.work.input_recoveries += 1;
        Ok(())
    }
    /// Only the canonical persistence owner invokes this after the SAME WAL group
    /// containing every phase.change is durable. Calling it is not publication.
    pub fn acknowledge_after_durable(
        &mut self,
        world: &mut World,
        phase: &LightingPhase,
    ) -> Result<(), ServerLightingError> {
        let Some(token) = phase.durable_token else {
            return Ok(());
        };
        if self.awaiting != Some(token) {
            return Err(ServerLightingError::InvalidDurableToken);
        }
        world.acknowledge_cursor(self.capture, token.1)?;
        self.awaiting = None;
        Ok(())
    }
    pub fn detach(self, world: &mut World) -> Result<(), ServerLightingError> {
        if self.awaiting.is_some() {
            return Err(ServerLightingError::AwaitingDurability);
        }
        self.engine.detach(world);
        world.remove_cursor(self.capture);
        Ok(())
    }
}
