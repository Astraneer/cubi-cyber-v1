//! Private LIGHT continuation. The engine supplies sparse queue operations;
//! this envelope adds only changed authority/sky obligations. No client data.
use super::*;

const MAX_KEYS: usize = 65_536;
const MAX_BYTES: usize = save::MAX_RUNTIME_BYTES;
const BAD: &str = "lighting continuation codec/state";

fn invalid() -> ServerError {
    ServerError::Invalid(BAD)
}
fn page(out: &mut Vec<u8>, p: PageId) {
    out.extend_from_slice(&p.space.0.to_le_bytes());
    out.extend_from_slice(&p.x.to_le_bytes());
    out.extend_from_slice(&p.y.to_le_bytes());
    out.extend_from_slice(&p.z.to_le_bytes());
}
fn column(out: &mut Vec<u8>, c: (i32, i32)) {
    out.extend_from_slice(&c.0.to_le_bytes());
    out.extend_from_slice(&c.1.to_le_bytes());
}
fn count(out: &mut Vec<u8>, n: usize) -> Result<(), ServerError> {
    if n > MAX_KEYS {
        return Err(invalid());
    }
    out.extend_from_slice(&(n as u32).to_le_bytes());
    Ok(())
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ServerError> {
        let (head, tail) = self.0.split_at_checked(n).ok_or_else(invalid)?;
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, ServerError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, ServerError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| invalid())?,
        ))
    }
    fn count(&mut self) -> Result<usize, ServerError> {
        let n = self.u32()? as usize;
        if n > MAX_KEYS {
            return Err(invalid());
        }
        Ok(n)
    }
    fn column(&mut self) -> Result<(i32, i32), ServerError> {
        Ok((self.u32()? as i32, self.u32()? as i32))
    }
    fn page(&mut self) -> Result<PageId, ServerError> {
        Ok(PageId {
            space: SpaceId(self.u32()?),
            x: self.u32()? as i32,
            y: self.u32()? as i32,
            z: self.u32()? as i32,
        })
    }
    fn boolean(&mut self) -> Result<bool, ServerError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid()),
        }
    }
}

impl LightOwner {
    pub(crate) fn clear_recovery_ops(&mut self) {
        self.durable_ops.clear();
    }
    pub(crate) fn observe_changes(
        &mut self,
        world: &World,
        changes: &[WorldChange],
    ) -> Result<(), ServerError> {
        self.engine
            .observe_changes(world, changes)
            .map_err(|_| invalid())?;
        self.observe_columns(changes, true);
        Ok(())
    }
    pub(crate) fn observe_columns(&mut self, changes: &[WorldChange], update_sky: bool) {
        for c in changes.iter().flat_map(|c| &c.pages) {
            let key = (c.page.space, c.page.x, c.page.z);
            if c.created {
                self.columns.entry(key).or_default().insert(c.page);
            }
            if c.removed {
                if let Some(pages) = self.columns.get_mut(&key) {
                    pages.remove(&c.page);
                    if pages.is_empty() {
                        self.columns.remove(&key);
                    }
                }
                if update_sky && self.sky_pending.remove(&c.page) {
                    self.record_page(3, c.page);
                }
            }
        }
    }
    pub(crate) fn queue_sky(&mut self, p: PageId) {
        if self.sky_pending.insert(p) {
            self.record_page(2, p);
        }
    }
    pub(crate) fn record_page(&mut self, tag: u8, p: PageId) {
        self.durable_ops.push(tag);
        page(&mut self.durable_ops, p);
    }
    pub(crate) fn record_column(&mut self, tag: u8, c: (i32, i32)) {
        self.durable_ops.push(tag);
        column(&mut self.durable_ops, c);
    }
    pub(crate) fn record_sources(&mut self) {
        self.durable_ops.push(4);
        self.durable_ops.extend_from_slice(&self.sections_stamp);
        self.durable_ops
            .extend_from_slice(&(self.source_pending.len() as u32).to_le_bytes());
        for c in &self.source_pending {
            column(&mut self.durable_ops, *c);
        }
    }
    pub(crate) fn snapshot(&self) -> Result<save::RuntimeRecord, ServerError> {
        if self.pending.is_some() || !self.durable_ops.is_empty() {
            return Err(invalid());
        }
        let engine = self.engine.snapshot().map_err(|_| invalid())?;
        let mut out = Vec::with_capacity(engine.len() + 64);
        out.extend_from_slice(b"LOS1");
        out.extend_from_slice(&(engine.len() as u32).to_le_bytes());
        out.extend_from_slice(&engine);
        out.push(u8::from(self.sky_ready));
        out.extend_from_slice(&self.sections_stamp);
        count(&mut out, self.sky_pending.len())?;
        for p in &self.sky_pending {
            page(&mut out, *p);
        }
        count(&mut out, self.sourced.len())?;
        for c in &self.sourced {
            column(&mut out, *c);
        }
        count(&mut out, self.source_pending.len())?;
        for c in &self.source_pending {
            column(&mut out, *c);
        }
        if out.len() > MAX_BYTES {
            return Err(invalid());
        }
        Ok(save::RuntimeRecord {
            kind: save::RUNTIME_LIGHT,
            payload: out,
        })
    }
    pub(crate) fn restore(
        world: &mut World,
        budget: world::lighting::LightBudget,
        bytes: &[u8],
    ) -> Result<Self, ServerError> {
        if bytes.len() > MAX_BYTES {
            return Err(invalid());
        }
        let mut r = Reader(bytes);
        if r.take(4)? != b"LOS1" {
            return Err(invalid());
        }
        let n = r.u32()? as usize;
        let engine = ServerLighting::restore(
            world,
            ServerLightingConfig {
                max_bootstrap_pages: MAX_KEYS,
                step: budget,
                ..ServerLightingConfig::default()
            },
            r.take(n)?,
        )
        .map_err(|_| invalid())?;
        let sky_ready = r.boolean()?;
        let sections_stamp = r.take(32)?.try_into().map_err(|_| invalid())?;
        let mut sky_pending = BTreeSet::new();
        for _ in 0..r.count()? {
            if !sky_pending.insert(r.page()?) {
                return Err(invalid());
            }
        }
        let mut sourced = BTreeSet::new();
        for _ in 0..r.count()? {
            if !sourced.insert(r.column()?) {
                return Err(invalid());
            }
        }
        let mut source_pending = VecDeque::new();
        let mut unique = BTreeSet::new();
        for _ in 0..r.count()? {
            let c = r.column()?;
            if !unique.insert(c) || sourced.contains(&c) {
                return Err(invalid());
            }
            source_pending.push_back(c);
        }
        if !r.0.is_empty() {
            return Err(invalid());
        }
        let mut columns: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        for (p, _) in world.pages() {
            columns.entry((p.space, p.x, p.z)).or_default().insert(p);
        }
        Ok(Self {
            engine,
            pending: None,
            columns,
            sky_pending,
            sky_ready,
            sourced,
            source_pending,
            sections_revision: 0,
            sections_stamp,
            durable_ops: Vec::new(),
        })
    }
    pub(crate) fn take_delta(&mut self) -> Result<Option<save::RuntimeRecord>, ServerError> {
        let engine = self.engine.take_delta().map_err(|_| invalid())?;
        if engine.is_none() && self.durable_ops.is_empty() {
            return Ok(None);
        }
        let engine = engine.as_deref().unwrap_or_default();
        let length = 8usize
            .checked_add(engine.len())
            .and_then(|n| n.checked_add(self.durable_ops.len()))
            .ok_or_else(invalid)?;
        if length > MAX_BYTES {
            return Err(invalid());
        }
        let mut out = Vec::with_capacity(length);
        out.extend_from_slice(b"LOD1");
        out.extend_from_slice(&(engine.len() as u32).to_le_bytes());
        out.extend_from_slice(engine);
        out.extend_from_slice(&self.durable_ops);
        self.durable_ops.clear();
        Ok(Some(save::RuntimeRecord {
            kind: save::RUNTIME_LIGHT,
            payload: out,
        }))
    }
    pub(crate) fn replay_delta(&mut self, bytes: &[u8]) -> Result<(), ServerError> {
        if bytes.len() > MAX_BYTES {
            return Err(invalid());
        }
        let mut r = Reader(bytes);
        if r.take(4)? != b"LOD1" {
            return Err(invalid());
        }
        let n = r.u32()? as usize;
        if n != 0 {
            self.engine
                .replay_delta(r.take(n)?)
                .map_err(|_| invalid())?;
        }
        while !r.0.is_empty() {
            match r.u8()? {
                1 => self.sky_ready = r.boolean()?,
                2 => {
                    if !self.sky_pending.insert(r.page()?) || self.sky_pending.len() > MAX_KEYS {
                        return Err(invalid());
                    }
                }
                3 => {
                    if !self.sky_pending.remove(&r.page()?) {
                        return Err(invalid());
                    }
                }
                4 => {
                    self.sections_stamp = r.take(32)?.try_into().map_err(|_| invalid())?;
                    self.source_pending.clear();
                    let mut unique = BTreeSet::new();
                    for _ in 0..r.count()? {
                        let c = r.column()?;
                        if !unique.insert(c) || self.sourced.contains(&c) {
                            return Err(invalid());
                        }
                        self.source_pending.push_back(c);
                    }
                }
                5 => {
                    if !self.sourced.remove(&r.column()?) {
                        return Err(invalid());
                    }
                }
                6 => {
                    let c = r.column()?;
                    if self.source_pending.pop_front() != Some(c)
                        || !self.sourced.insert(c)
                        || self.sourced.len() > MAX_KEYS
                    {
                        return Err(invalid());
                    }
                }
                _ => return Err(invalid()),
            }
        }
        Ok(())
    }
}

impl Server {
    pub(crate) fn prepare_light_batch(&mut self, batch: &mut Batch) -> Result<(), ServerError> {
        if batch.runtimes.iter().any(|r| r.kind == save::RUNTIME_LIGHT) {
            return Ok(());
        }
        self.lighting.observe_changes(&self.world, &batch.changes)?;
        if let Some(record) = self.lighting.take_delta()? {
            batch.runtimes.push(record);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_sky_and_source_operations_restore_exact_owner_metadata() {
        let budget = world::lighting::LightBudget::default();
        let mut world = World::new(rules::properties, 4);
        let mut live = LightOwner::attach(&mut world, budget).unwrap();
        let baseline = live.snapshot().unwrap();
        let mut recovered_world = World::new(rules::properties, 4);
        let mut recovered =
            LightOwner::restore(&mut recovered_world, budget, &baseline.payload).unwrap();
        let a = PageId {
            space: SpaceId(2),
            x: -4,
            y: 8,
            z: 9,
        };
        let b = PageId { x: 5, ..a };
        live.sky_ready = true;
        live.durable_ops.extend_from_slice(&[1, 1]);
        assert!(
            live.snapshot().is_err(),
            "flush metadata before checkpointing"
        );
        live.queue_sky(a);
        live.queue_sky(b);
        live.sections_stamp = [7; 32];
        live.source_pending.extend([(-2, 3), (5, 6)]);
        live.record_sources();
        let column = live.source_pending.pop_front().unwrap();
        live.sourced.insert(column);
        live.record_column(6, column);
        live.sky_pending.remove(&a);
        live.record_page(3, a);
        let delta = live.take_delta().unwrap().unwrap();
        // This is sparse metadata, not a copy of the frontier/free-slot table.
        assert!(delta.payload.len() < 160);
        recovered.replay_delta(&delta.payload).unwrap();
        assert_eq!(recovered.snapshot().unwrap(), live.snapshot().unwrap());
        assert!(live.take_delta().unwrap().is_none());
        assert!(recovered.take_delta().unwrap().is_none());
        assert!(recovered.replay_delta(&delta.payload).is_err());
    }

    #[test]
    fn continuation_snapshot_rejects_trailing_and_truncated_metadata() {
        let budget = world::lighting::LightBudget::default();
        let mut world = World::new(rules::properties, 4);
        let owner = LightOwner::attach(&mut world, budget).unwrap();
        let bytes = owner.snapshot().unwrap().payload;
        for damaged in [bytes[..bytes.len() - 1].to_vec(), {
            let mut bytes = bytes.clone();
            bytes.push(0);
            bytes
        }] {
            let mut world = World::new(rules::properties, 4);
            assert!(LightOwner::restore(&mut world, budget, &damaged).is_err());
        }
    }

    #[test]
    fn legacy_wal_gets_a_light_baseline_before_the_first_continuation_delta() {
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/test-light-migration")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let directory = Directory(path);
        let world = World::new(rules::properties, 4);
        let (mut store, _) = Store::open(&directory.0).unwrap();
        store
            .load_or_create_private_entropy(|| Ok([7; 32]))
            .unwrap();
        store
            .checkpoint(&Snapshot {
                record_sequence: 0,
                server_tick: Tick(0),
                public_seed: 1,
                generator_version: r#gen::GENERATOR_VERSION,
                rules_version: net::RULES_VERSION,
                world: world.checkpoint().to_restore_state(),
                identities: Vec::new(),
                players: Vec::new(),
                dedup: Vec::new(),
                runtimes: Vec::new(),
            })
            .unwrap();
        store.append(&Batch::empty(Tick(1))).unwrap();
        drop(store);
        let mut config = ServerConfig::new(&directory.0);
        config.generation_enabled = false;
        let server = Server::open(config.clone()).unwrap();
        let expected = server.lighting.snapshot().unwrap();
        drop(server);
        let (store, recovered) = Store::open(&directory.0).unwrap();
        let snapshot = recovered.snapshot.unwrap();
        assert_eq!(snapshot.record_sequence, 1);
        assert_eq!(
            snapshot
                .runtimes
                .iter()
                .find(|r| r.kind == save::RUNTIME_LIGHT),
            Some(&expected)
        );
        drop(store);
        let server = Server::open(config).unwrap();
        assert_eq!(server.current_tick(), Tick(1));
        assert_eq!(server.lighting.snapshot().unwrap(), expected);
    }
}
