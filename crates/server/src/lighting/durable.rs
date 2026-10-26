//! Engine bytes plus the finite ordered bootstrap obligation. The enclosing
//! LightOwner persists sky-proof/source obligations in the same kind8 record.
use super::*;
use world::lighting::durable::{Delta, MAX_BYTES, Snapshot};
fn invalid(reason: &'static str) -> ServerLightingError {
    ServerLightingError::Engine(LightingError::Persistence(reason))
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ServerLightingError> {
        let (a, b) = self
            .0
            .split_at_checked(n)
            .ok_or(invalid("adapter truncated"))?;
        self.0 = b;
        Ok(a)
    }
    fn u32(&mut self) -> Result<u32, ServerLightingError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("length"),
        ))
    }
    fn u64(&mut self) -> Result<u64, ServerLightingError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("length"),
        ))
    }
    fn pages(&mut self, limit: usize) -> Result<Vec<PageId>, ServerLightingError> {
        let count = self.u32()? as usize;
        if count > limit || count > self.0.len() / 16 {
            return Err(invalid("adapter pages"));
        }
        let mut pages = Vec::with_capacity(count);
        for _ in 0..count {
            pages.push(PageId {
                space: SpaceId(self.u32()?),
                x: self.u32()? as i32,
                y: self.u32()? as i32,
                z: self.u32()? as i32,
            });
        }
        Ok(pages)
    }
}
fn pages(out: &mut Vec<u8>, pages: impl ExactSizeIterator<Item = PageId>) {
    out.extend_from_slice(&(pages.len() as u32).to_le_bytes());
    for p in pages {
        out.extend_from_slice(&p.space.0.to_le_bytes());
        out.extend_from_slice(&p.x.to_le_bytes());
        out.extend_from_slice(&p.y.to_le_bytes());
        out.extend_from_slice(&p.z.to_le_bytes());
    }
}
impl ServerLighting {
    pub fn attach_durable(
        world: &mut World,
        config: ServerLightingConfig,
    ) -> Result<Self, ServerLightingError> {
        let mut owner = Self::attach(world, config)?;
        owner.engine.enable_durable_inputs()?;
        // Journal-loss reseeding is exclusively the legacy adapter. Durable
        // inputs retain each semantic obligation independently of journal age.
        owner.recovery_sources.clear();
        owner.durable_revision = Some(0);
        Ok(owner)
    }
    pub fn observe_changes(
        &mut self,
        world: &World,
        changes: &[WorldChange],
    ) -> Result<(), ServerLightingError> {
        if self.durable_revision.is_none() {
            return Err(invalid("adapter input mode"));
        }
        for change in changes {
            self.engine.observe_durable_change(world, change)?;
        }
        Ok(())
    }
    pub fn snapshot(&self) -> Result<Vec<u8>, ServerLightingError> {
        let revision = self.durable_revision.ok_or(invalid("adapter input mode"))?;
        let engine = self.engine.durable_snapshot()?.encode()?;
        let length = 20usize
            .checked_add(engine.len())
            .and_then(|n| n.checked_add(self.bootstrap.len() * 16))
            .ok_or(ServerLightingError::Capacity)?;
        if length > MAX_BYTES {
            return Err(ServerLightingError::Capacity);
        }
        let mut out = Vec::with_capacity(length);
        out.extend_from_slice(b"LCS1");
        out.extend_from_slice(&revision.to_le_bytes());
        out.extend_from_slice(&(engine.len() as u32).to_le_bytes());
        out.extend_from_slice(&engine);
        pages(&mut out, self.bootstrap.iter().copied());
        Ok(out)
    }
    pub fn restore(
        world: &mut World,
        config: ServerLightingConfig,
        bytes: &[u8],
    ) -> Result<Self, ServerLightingError> {
        if bytes.len() > MAX_BYTES {
            return Err(ServerLightingError::Capacity);
        }
        let mut r = Reader(bytes);
        if r.take(4)? != b"LCS1" {
            return Err(invalid("adapter snapshot header"));
        }
        let revision = r.u64()?;
        let n = r.u32()? as usize;
        let snapshot = Snapshot::decode(r.take(n)?)?;
        let bootstrap = r.pages(config.max_bootstrap_pages)?;
        if !r.0.is_empty()
            || bootstrap.iter().copied().collect::<BTreeSet<_>>().len() != bootstrap.len()
        {
            return Err(invalid("adapter snapshot pages"));
        }
        let engine = LightingEngine::restore_durable(world, config.engine, snapshot)?;
        let capture = world.register_cursor();
        Ok(Self {
            config,
            engine,
            capture,
            bootstrap: bootstrap.into(),
            recovery_sources: BTreeMap::new(),
            awaiting: None,
            nonce: 0,
            work: ServerLightWork::default(),
            durable_revision: Some(revision),
            bootstrap_removed: Vec::new(),
        })
    }
    /// Queue mutations already belong to the live owner; this returns their
    /// single WAL delta. Replay is for recovery, never the live fsync ACK.
    pub fn take_delta(&mut self) -> Result<Option<Vec<u8>>, ServerLightingError> {
        let before = self.durable_revision.ok_or(invalid("adapter input mode"))?;
        let engine = self
            .engine
            .take_durable_delta()?
            .map(|d| d.encode())
            .transpose()?;
        if engine.is_none() && self.bootstrap_removed.is_empty() {
            return Ok(None);
        }
        let after = before.checked_add(1).ok_or(ServerLightingError::Capacity)?;
        let engine = engine.as_deref().unwrap_or_default();
        let length = 28usize
            .checked_add(engine.len())
            .and_then(|n| n.checked_add(self.bootstrap_removed.len() * 16))
            .ok_or(ServerLightingError::Capacity)?;
        if length > MAX_BYTES {
            return Err(ServerLightingError::Capacity);
        }
        let mut out = Vec::with_capacity(length);
        out.extend_from_slice(b"LCD1");
        out.extend_from_slice(&before.to_le_bytes());
        out.extend_from_slice(&after.to_le_bytes());
        out.extend_from_slice(&(engine.len() as u32).to_le_bytes());
        out.extend_from_slice(engine);
        pages(&mut out, self.bootstrap_removed.iter().copied());
        self.bootstrap_removed.clear();
        self.durable_revision = Some(after);
        Ok(Some(out))
    }
    pub fn replay_delta(&mut self, bytes: &[u8]) -> Result<(), ServerLightingError> {
        if bytes.len() > MAX_BYTES {
            return Err(ServerLightingError::Capacity);
        }
        let mut r = Reader(bytes);
        if r.take(4)? != b"LCD1" {
            return Err(invalid("adapter delta header"));
        }
        let before = r.u64()?;
        let after = r.u64()?;
        if self.durable_revision != Some(before)
            || before.checked_add(1) != Some(after)
            || !self.bootstrap_removed.is_empty()
            || self.awaiting.is_some()
        {
            return Err(invalid("adapter stale delta"));
        }
        let n = r.u32()? as usize;
        let engine = if n == 0 {
            None
        } else {
            Some(Delta::decode(r.take(n)?)?)
        };
        let removed = r.pages(self.config.max_bootstrap_pages)?;
        if !r.0.is_empty()
            || !self
                .bootstrap
                .iter()
                .copied()
                .take(removed.len())
                .eq(removed.iter().copied())
            || removed.len() > self.bootstrap.len()
        {
            return Err(invalid("adapter delta bootstrap"));
        }
        if let Some(engine) = engine {
            self.engine.apply_durable_delta(engine)?;
        }
        for _ in removed {
            self.bootstrap.pop_front();
        }
        self.durable_revision = Some(after);
        Ok(())
    }
}
