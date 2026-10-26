//! Authoritative CPU light adapter. Returned derived records belong to the same
//! durable server phase as gameplay; no renderer or generation call is made here.
use core_types::{BlockPos, PageId, SpaceId, Tick};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use world::{
    CursorError, CursorId, World, WorldChange,
    lighting::{
        LightBudget, LightIngestBudget, LightPriority, LightProgress, LightingEngine,
        LightingError, LightingLimits, SkyAccess,
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
    awaiting: Option<(u64, u64)>,
    nonce: u64,
    work: ServerLightWork,
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
        Ok(Self {
            config,
            engine,
            capture,
            bootstrap,
            awaiting: None,
            nonce: 0,
            work: ServerLightWork::default(),
        })
    }
    /// Supply cached loaded page keys affected by a new immutable sky proof.
    /// The caller owns column→loaded-page metadata; this never scans cold World.
    /// A blocked page is returned as false and must remain pending at the caller.
    pub fn request_sky_page(&mut self, page: PageId, tick: Tick) -> bool {
        let accepted = self
            .engine
            .queue_page(page, tick, LightPriority::Interactive)
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
