//! Private kind-4 RUN1 payloads. Bounds precede variable-vector allocations.
use super::*;
use machines::grid::{Endpoint, NetworkSnapshot, NodeSnapshot, SlotSnapshot};
use machines::grid::{IndexedSlot, TopologyEdit, TopologyRecords};
const MAX_BYTES: usize = save::MAX_RUNTIME_BYTES;
struct Writer {
    bytes: Vec<u8>,
    overflow: bool,
}
impl Writer {
    fn new(magic: &[u8; 4]) -> Self {
        Self {
            bytes: magic.to_vec(),
            overflow: false,
        }
    }
    fn raw(&mut self, bytes: &[u8]) {
        if self.bytes.len().saturating_add(bytes.len()) > MAX_BYTES {
            self.overflow = true;
        } else if !self.overflow {
            self.bytes.extend_from_slice(bytes);
        }
    }
    fn u8(&mut self, n: u8) {
        self.raw(&[n]);
    }
    fn u16(&mut self, n: u16) {
        self.raw(&n.to_le_bytes());
    }
    fn u32(&mut self, n: u32) {
        self.raw(&n.to_le_bytes());
    }
    fn u64(&mut self, n: u64) {
        self.raw(&n.to_le_bytes());
    }
    fn count(&mut self, n: usize) {
        if let Ok(n) = u32::try_from(n) {
            self.u32(n);
        } else {
            self.overflow = true;
        }
    }
    fn position(&mut self, p: [i32; 3]) {
        for n in p {
            self.u32(n as u32);
        }
    }
    fn anchor(&mut self, p: BlockPos) {
        self.u32(p.space.0);
        self.position([p.x, p.y, p.z]);
    }
    fn key(&mut self, key: NetworkKey) {
        self.u32(key.space.0);
        self.u32(key.network.slot);
        self.u32(key.network.generation);
    }
    fn meta(&mut self, m: MachineMeta) {
        self.u64(m.id.0);
        self.anchor(m.anchor);
        self.u64(m.order);
        self.u64(m.last_tick.0);
        self.u8(u8::from(m.active));
    }
    fn pending(&mut self, p: Pending) {
        match p.event {
            Event::Machine(id) => {
                self.u8(0);
                self.u64(id.0);
            }
            Event::Network(key) => {
                self.u8(1);
                self.key(key);
            }
        }
        self.u64(p.due.0);
        self.u64(p.first_due.0);
        self.u64(p.order);
    }
    fn owner_state(&mut self, state: Option<OwnerState>) {
        self.u8(u8::from(state.is_some()));
        if let Some(s) = state {
            self.u64(s.tick.0);
            self.u8(s.stage as u8);
            self.u8(u8::from(s.prepared_after.is_some()));
            if let Some((due, phase, order)) = s.prepared_after {
                self.u64(due.0);
                self.u8(phase);
                self.u64(order);
            }
        }
    }
    fn reservoir(&mut self, s: Reservoir) {
        self.u8(s.substance);
        self.u64(s.stored);
        self.u16(s.glow);
        self.u64(s.flow);
        self.u8(u8::from(s.sent));
    }
    fn resource(&mut self, r: Resource) {
        self.u8(match r {
            Resource::Energy => 0,
            Resource::Fluid => 1,
            Resource::Gas => 2,
        });
    }
    fn node(&mut self, n: &NodeSnapshot) {
        self.position(n.position);
        self.u32(n.network.slot);
        self.u32(n.network.generation);
        self.resource(n.resource);
        self.u8(n.links);
    }
    fn slot(&mut self, s: &SlotSnapshot) {
        self.u32(s.generation);
        self.u8(u8::from(s.network.is_some()));
        if let Some(n) = &s.network {
            self.resource(n.resource);
            self.reservoir(n.reservoir);
            self.count(n.members.len());
            for &p in &n.members {
                self.position(p);
            }
            self.count(n.endpoints.len());
            for e in &n.endpoints {
                self.u64(e.owner);
                self.position(e.position);
                self.u8(e.face as u8);
                self.u8(u8::from(e.pull));
            }
        }
    }
    fn grid(&mut self, g: &GridSnapshot) {
        self.count(g.nodes.len());
        for n in &g.nodes {
            self.node(n);
        }
        self.count(g.slots.len());
        for s in &g.slots {
            self.slot(s);
        }
    }
    fn finish(self) -> Result<Vec<u8>, MachineError> {
        if self.overflow {
            Err(MachineError::Bounds)
        } else {
            Ok(self.bytes)
        }
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
    config: Config,
}
impl Reader<'_> {
    fn raw<const N: usize>(&mut self) -> Result<[u8; N], MachineError> {
        let end = self.cursor.checked_add(N).ok_or(MachineError::Bounds)?;
        let bytes = self
            .bytes
            .get(self.cursor..end)
            .ok_or(MachineError::Encoding)?
            .try_into()
            .map_err(|_| MachineError::Encoding)?;
        self.cursor = end;
        Ok(bytes)
    }
    fn u8(&mut self) -> Result<u8, MachineError> {
        Ok(self.raw::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, MachineError> {
        Ok(u16::from_le_bytes(self.raw()?))
    }
    fn u32(&mut self) -> Result<u32, MachineError> {
        Ok(u32::from_le_bytes(self.raw()?))
    }
    fn u64(&mut self) -> Result<u64, MachineError> {
        Ok(u64::from_le_bytes(self.raw()?))
    }
    fn boolean(&mut self) -> Result<bool, MachineError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(MachineError::Encoding),
        }
    }
    fn count(&mut self, maximum: usize, minimum_bytes: usize) -> Result<usize, MachineError> {
        let n = self.u32()? as usize;
        if n > maximum
            || n.checked_mul(minimum_bytes)
                .is_none_or(|needed| needed > self.bytes.len().saturating_sub(self.cursor))
        {
            Err(MachineError::Bounds)
        } else {
            Ok(n)
        }
    }
    fn position(&mut self) -> Result<[i32; 3], MachineError> {
        Ok([self.u32()? as i32, self.u32()? as i32, self.u32()? as i32])
    }
    fn anchor(&mut self) -> Result<BlockPos, MachineError> {
        let space = SpaceId(self.u32()?);
        let [x, y, z] = self.position()?;
        Ok(BlockPos { space, x, y, z })
    }
    fn key(&mut self) -> Result<NetworkKey, MachineError> {
        Ok(NetworkKey {
            space: SpaceId(self.u32()?),
            network: NetworkId {
                slot: self.u32()?,
                generation: self.u32()?,
            },
        })
    }
    fn meta(&mut self) -> Result<MachineMeta, MachineError> {
        Ok(MachineMeta {
            id: ComponentId(self.u64()?),
            anchor: self.anchor()?,
            order: self.u64()?,
            last_tick: Tick(self.u64()?),
            active: self.boolean()?,
        })
    }
    fn pending(&mut self) -> Result<Pending, MachineError> {
        let event = match self.u8()? {
            0 => Event::Machine(ComponentId(self.u64()?)),
            1 => Event::Network(self.key()?),
            _ => return Err(MachineError::Encoding),
        };
        Ok(Pending {
            event,
            due: Tick(self.u64()?),
            first_due: Tick(self.u64()?),
            order: self.u64()?,
        })
    }
    fn owner_state(&mut self) -> Result<Option<OwnerState>, MachineError> {
        if !self.boolean()? {
            return Ok(None);
        }
        let tick = Tick(self.u64()?);
        let stage = match self.u8()? {
            0 => OwnerStage::Prepare,
            1 => OwnerStage::Machines,
            2 => OwnerStage::Resources,
            3 => OwnerStage::Transit,
            _ => return Err(MachineError::Encoding),
        };
        let prepared_after = if self.boolean()? {
            Some((Tick(self.u64()?), self.u8()?, self.u64()?))
        } else {
            None
        };
        Ok(Some(OwnerState {
            tick,
            stage,
            prepared_after,
        }))
    }
    fn reservoir(&mut self) -> Result<Reservoir, MachineError> {
        Ok(Reservoir {
            substance: self.u8()?,
            stored: self.u64()?,
            glow: self.u16()?,
            flow: self.u64()?,
            sent: self.boolean()?,
        })
    }
    fn resource(&mut self) -> Result<Resource, MachineError> {
        match self.u8()? {
            0 => Ok(Resource::Energy),
            1 => Ok(Resource::Fluid),
            2 => Ok(Resource::Gas),
            _ => Err(MachineError::Encoding),
        }
    }
    fn node(&mut self) -> Result<NodeSnapshot, MachineError> {
        Ok(NodeSnapshot {
            position: self.position()?,
            network: NetworkId {
                slot: self.u32()?,
                generation: self.u32()?,
            },
            resource: self.resource()?,
            links: self.u8()?,
        })
    }
    fn slot(&mut self) -> Result<SlotSnapshot, MachineError> {
        let generation = self.u32()?;
        let network = if self.boolean()? {
            let resource = self.resource()?;
            let reservoir = self.reservoir()?;
            let count = self.count(self.config.transmitters, 12)?;
            let mut members = Vec::with_capacity(count);
            for _ in 0..count {
                members.push(self.position()?);
            }
            let count = self.count(self.config.endpoints, 22)?;
            let mut endpoints = Vec::with_capacity(count);
            for _ in 0..count {
                let owner = self.u64()?;
                let position = self.position()?;
                let face = *Face::ALL
                    .get(self.u8()? as usize)
                    .ok_or(MachineError::Encoding)?;
                let pull = self.boolean()?;
                endpoints.push(Endpoint {
                    owner,
                    position,
                    face,
                    pull,
                });
            }
            Some(NetworkSnapshot {
                resource,
                reservoir,
                members,
                endpoints,
            })
        } else {
            None
        };
        Ok(SlotSnapshot {
            generation,
            network,
        })
    }
    fn grid(&mut self) -> Result<GridSnapshot, MachineError> {
        let count = self.count(self.config.transmitters, 22)?;
        let mut nodes = Vec::with_capacity(count);
        for _ in 0..count {
            nodes.push(self.node()?);
        }
        let count = self.count(self.config.transmitters, 5)?;
        let mut slots = Vec::with_capacity(count);
        for _ in 0..count {
            slots.push(self.slot()?);
        }
        Ok(GridSnapshot { nodes, slots })
    }
    fn blob(&mut self, maximum: usize) -> Result<&[u8], MachineError> {
        let n = self.count(maximum, 1)?;
        let end = self.cursor.checked_add(n).ok_or(MachineError::Bounds)?;
        let bytes = self
            .bytes
            .get(self.cursor..end)
            .ok_or(MachineError::Encoding)?;
        self.cursor = end;
        Ok(bytes)
    }
    fn finish(&self) -> Result<(), MachineError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(MachineError::Encoding)
        }
    }
}
impl RuntimeSnapshot {
    pub fn encode(&self) -> Result<Vec<u8>, MachineError> {
        let mut w = Writer::new(b"MSR1");
        w.u64(self.revision);
        w.u64(self.next_order);
        w.count(self.machines.len());
        for &m in &self.machines {
            w.meta(m);
        }
        w.count(self.events.len());
        for &p in &self.events {
            w.pending(p);
        }
        w.count(self.grids.len());
        for (space, grid) in &self.grids {
            w.u32(space.0);
            w.grid(grid);
        }
        w.count(self.network_orders.len());
        for &(key, order) in &self.network_orders {
            w.key(key);
            w.u64(order);
        }
        w.count(self.producers.len());
        for (key, ids) in &self.producers {
            w.key(*key);
            w.count(ids.len());
            for id in ids {
                w.u64(id.0);
            }
        }
        if !self.configs.is_empty() {
            w.raw(b"XCFG");
            w.count(self.configs.len());
            for &config in &self.configs {
                w.transmitter(config);
            }
        }
        if self.transport != transport::TransportSnapshot::default() {
            let bytes = self.transport.encode(Config {
                packets: 65536,
                transmitters: 1_000_000,
                ..Config::default()
            })?;
            w.raw(b"XPKT");
            w.count(bytes.len());
            w.raw(&bytes);
        }
        if self.topology_queue != crate::topology_queue::Snapshot::default() {
            let bytes = self
                .topology_queue
                .encode()
                .map_err(|_| MachineError::Encoding)?;
            w.raw(b"XTPQ");
            w.count(bytes.len());
            w.raw(&bytes);
        }
        if self.owner != OwnerSnapshot::default() {
            w.raw(b"XPH1");
            w.owner_state(self.owner.state);
            w.count(self.owner.ready.len());
            for &p in &self.owner.ready {
                w.pending(p);
            }
        }
        w.finish()
    }
    pub fn decode(bytes: &[u8], config: Config) -> Result<Self, MachineError> {
        if bytes.len() > MAX_BYTES {
            return Err(MachineError::Bounds);
        }
        let mut r = Reader {
            bytes,
            cursor: 0,
            config,
        };
        if r.raw::<4>()? != *b"MSR1" {
            return Err(MachineError::Encoding);
        }
        let revision = r.u64()?;
        let next_order = r.u64()?;
        let count = r.count(config.machines, 41)?;
        let mut machines = Vec::with_capacity(count);
        for _ in 0..count {
            machines.push(r.meta()?);
        }
        let count = r.count(config.events, 33)?;
        let mut events = Vec::with_capacity(count);
        for _ in 0..count {
            events.push(r.pending()?);
        }
        let count = r.count(64, 12)?;
        let mut grids = Vec::with_capacity(count);
        for _ in 0..count {
            grids.push((SpaceId(r.u32()?), r.grid()?));
        }
        let count = r.count(config.transmitters, 20)?;
        let mut network_orders = Vec::with_capacity(count);
        for _ in 0..count {
            network_orders.push((r.key()?, r.u64()?));
        }
        let count = r.count(config.transmitters, 16)?;
        let mut producers = Vec::with_capacity(count);
        for _ in 0..count {
            let key = r.key()?;
            let count = r.count(config.machines, 8)?;
            let mut ids = Vec::with_capacity(count);
            for _ in 0..count {
                ids.push(ComponentId(r.u64()?));
            }
            producers.push((key, ids));
        }
        let mut configs = Vec::new();
        if r.peek(b"XCFG") {
            r.raw::<4>()?;
            let count = r.count(config.transmitters, 21)?;
            configs = Vec::with_capacity(count);
            for _ in 0..count {
                configs.push(r.transmitter()?);
            }
        }
        let transport = if r.peek(b"XPKT") {
            r.raw::<4>()?;
            transport::TransportSnapshot::decode(r.blob(transport::MAX_TRANSPORT_BYTES)?, config)?
        } else {
            transport::TransportSnapshot::default()
        };
        let topology_queue = if r.peek(b"XTPQ") {
            r.raw::<4>()?;
            crate::topology_queue::Snapshot::decode(
                r.blob(MAX_BYTES)?,
                topology_pending::PENDING_TOPOLOGY_CAPACITY,
            )
            .map_err(|_| MachineError::Encoding)?
        } else {
            crate::topology_queue::Snapshot::default()
        };
        let owner = if r.peek(b"XPH1") {
            r.raw::<4>()?;
            let state = r.owner_state()?;
            let count = r.count(config.events, 33)?;
            let mut ready = Vec::with_capacity(count);
            for _ in 0..count {
                ready.push(r.pending()?);
            }
            OwnerSnapshot { state, ready }
        } else {
            OwnerSnapshot::default()
        };
        r.finish()?;
        let snapshot = Self {
            revision,
            next_order,
            machines,
            events,
            grids,
            network_orders,
            producers,
            configs,
            transport,
            topology_queue,
            owner,
        };
        MachineRuntime::restore(config, &snapshot)?;
        Ok(snapshot)
    }
}
impl RuntimeDelta {
    pub fn is_empty(&self) -> bool {
        self.owner.is_empty()
            && self.machines.is_empty()
            && self.queue.is_empty()
            && self.reservoirs.is_empty()
            && self.topology.is_empty()
            && self.networks.is_empty()
            && self.endpoints.is_empty()
            && self.configs.is_empty()
            && self
                .transport
                .as_ref()
                .is_none_or(transport::TransportDelta::is_empty)
            && self
                .topology_queue
                .as_ref()
                .is_none_or(crate::topology_queue::Delta::is_empty)
            && self.next_order_before == self.next_order_after
    }
    pub fn encode(&self) -> Result<Vec<u8>, MachineError> {
        let mut w = Writer::new(b"MSD1");
        for n in [
            self.revision,
            self.next_revision,
            self.next_order_before,
            self.next_order_after,
        ] {
            w.u64(n);
        }
        w.count(self.machines.len());
        for p in &self.machines {
            w.u8(u8::from(p.before.is_some()));
            if let Some(m) = p.before {
                w.meta(m);
            }
            w.u8(u8::from(p.after.is_some()));
            if let Some(m) = p.after {
                w.meta(m);
            }
        }
        w.count(self.queue.len());
        for p in &self.queue {
            w.u8(u8::from(p.before.is_some()));
            if let Some(p) = p.before {
                w.pending(p);
            }
            w.u8(u8::from(p.after.is_some()));
            if let Some(p) = p.after {
                w.pending(p);
            }
        }
        w.count(self.reservoirs.len());
        for p in &self.reservoirs {
            w.key(p.key);
            w.reservoir(p.before);
            w.reservoir(p.after);
        }
        if !self.topology.is_empty()
            || !self.networks.is_empty()
            || !self.endpoints.is_empty()
            || !self.configs.is_empty()
        {
            w.raw(b"XTOP");
            w.count(self.topology.len());
            for p in &self.topology {
                w.u32(p.space.0);
                w.topology(&p.records);
            }
            w.count(self.networks.len());
            for p in &self.networks {
                w.u8(u8::from(p.before.is_some()));
                if let Some(meta) = &p.before {
                    w.network_meta(meta);
                }
                w.u8(u8::from(p.after.is_some()));
                if let Some(meta) = &p.after {
                    w.network_meta(meta);
                }
            }
            w.count(self.endpoints.len());
            for p in &self.endpoints {
                w.key(p.key);
                w.endpoints(&p.before);
                w.endpoints(&p.after);
            }
            w.count(self.configs.len());
            for p in &self.configs {
                w.u8(u8::from(p.before.is_some()));
                if let Some(config) = p.before {
                    w.transmitter(config);
                }
                w.u8(u8::from(p.after.is_some()));
                if let Some(config) = p.after {
                    w.transmitter(config);
                }
            }
        }
        if let Some(transport) = &self.transport {
            let bytes = transport.encode()?;
            w.raw(b"XPKT");
            w.count(bytes.len());
            w.raw(&bytes);
        }
        if let Some(queue) = &self.topology_queue {
            let bytes = queue.encode().map_err(|_| MachineError::Encoding)?;
            w.raw(b"XTPQ");
            w.count(bytes.len());
            w.raw(&bytes);
        }
        if self.owner.before.is_some() || self.owner.after.is_some() || !self.owner.ready.is_empty()
        {
            w.raw(b"XPH1");
            w.owner_state(self.owner.before);
            w.owner_state(self.owner.after);
            w.count(self.owner.ready.len());
            for p in &self.owner.ready {
                w.u8(u8::from(p.before.is_some()));
                if let Some(p) = p.before {
                    w.pending(p);
                }
                w.u8(u8::from(p.after.is_some()));
                if let Some(p) = p.after {
                    w.pending(p);
                }
            }
        }
        w.finish()
    }
    pub fn decode(bytes: &[u8], config: Config) -> Result<Self, MachineError> {
        if bytes.len() > MAX_BYTES {
            return Err(MachineError::Bounds);
        }
        let mut r = Reader {
            bytes,
            cursor: 0,
            config,
        };
        if r.raw::<4>()? != *b"MSD1" {
            return Err(MachineError::Encoding);
        }
        let revision = r.u64()?;
        let next_revision = r.u64()?;
        let next_order_before = r.u64()?;
        let next_order_after = r.u64()?;
        if revision.checked_add(1) != Some(next_revision) || next_order_after < next_order_before {
            return Err(MachineError::Encoding);
        }
        let count = r.count(config.machines, 43)?;
        let mut machines = Vec::with_capacity(count);
        for _ in 0..count {
            let before = if r.boolean()? { Some(r.meta()?) } else { None };
            let after = if r.boolean()? { Some(r.meta()?) } else { None };
            if before.is_none() && after.is_none() {
                return Err(MachineError::Encoding);
            }
            machines.push(MetaPatch { before, after });
        }
        let count = r.count(config.events, 35)?;
        let mut queue = Vec::with_capacity(count);
        for _ in 0..count {
            let before = if r.boolean()? {
                Some(r.pending()?)
            } else {
                None
            };
            let after = if r.boolean()? {
                Some(r.pending()?)
            } else {
                None
            };
            if before.is_none() && after.is_none() {
                return Err(MachineError::Encoding);
            }
            queue.push(QueuePatch { before, after });
        }
        let count = r.count(config.transmitters, 52)?;
        let mut reservoirs = Vec::with_capacity(count);
        for _ in 0..count {
            reservoirs.push(ReservoirPatch {
                key: r.key()?,
                before: r.reservoir()?,
                after: r.reservoir()?,
            });
        }
        let mut topology = Vec::new();
        let mut networks = Vec::new();
        let mut endpoints = Vec::new();
        let mut configs = Vec::new();
        if r.peek(b"XTOP") {
            r.raw::<4>()?;
            let count = r.count(64, 21)?;
            topology = Vec::with_capacity(count);
            for _ in 0..count {
                topology.push(TopologyPatch {
                    space: SpaceId(r.u32()?),
                    records: r.topology()?,
                });
            }
            let count = r.count(config.transmitters, 26)?;
            networks = Vec::with_capacity(count);
            for _ in 0..count {
                let before = if r.boolean()? {
                    Some(r.network_meta()?)
                } else {
                    None
                };
                let after = if r.boolean()? {
                    Some(r.network_meta()?)
                } else {
                    None
                };
                if before.is_none() && after.is_none() {
                    return Err(MachineError::Encoding);
                }
                networks.push(NetworkPatch { before, after });
            }
            let count = r.count(config.transmitters, 20)?;
            endpoints = Vec::with_capacity(count);
            for _ in 0..count {
                endpoints.push(EndpointPatch {
                    key: r.key()?,
                    before: r.endpoints()?,
                    after: r.endpoints()?,
                });
            }
            let count = r.count(config.transmitters, 23)?;
            configs = Vec::with_capacity(count);
            for _ in 0..count {
                let before = if r.boolean()? {
                    Some(r.transmitter()?)
                } else {
                    None
                };
                let after = if r.boolean()? {
                    Some(r.transmitter()?)
                } else {
                    None
                };
                if before.is_none() && after.is_none() {
                    return Err(MachineError::Encoding);
                }
                configs.push(ConfigPatch { before, after });
            }
        }
        let transport = if r.peek(b"XPKT") {
            r.raw::<4>()?;
            Some(transport::TransportDelta::decode(
                r.blob(transport::MAX_TRANSPORT_DELTA_BYTES)?,
                config,
            )?)
        } else {
            None
        };
        let topology_queue = if r.peek(b"XTPQ") {
            r.raw::<4>()?;
            Some(
                crate::topology_queue::Delta::decode(r.blob(MAX_BYTES)?)
                    .map_err(|_| MachineError::Encoding)?,
            )
        } else {
            None
        };
        let owner = if r.peek(b"XPH1") {
            r.raw::<4>()?;
            let before = r.owner_state()?;
            let after = r.owner_state()?;
            let count = r.count(config.events, 35)?;
            let mut ready = Vec::with_capacity(count);
            for _ in 0..count {
                let before = if r.boolean()? {
                    Some(r.pending()?)
                } else {
                    None
                };
                let after = if r.boolean()? {
                    Some(r.pending()?)
                } else {
                    None
                };
                if before.is_none() && after.is_none() {
                    return Err(MachineError::Encoding);
                }
                ready.push(QueuePatch { before, after });
            }
            barrier::OwnerDelta {
                before,
                after,
                ready,
            }
        } else {
            barrier::OwnerDelta::default()
        };
        r.finish()?;
        Ok(Self {
            revision,
            next_revision,
            next_order_before,
            next_order_after,
            machines,
            queue,
            reservoirs,
            topology,
            networks,
            endpoints,
            configs,
            transport,
            topology_queue,
            owner,
        })
    }
}

impl Writer {
    fn transmitter(&mut self, c: TransmitterConfig) {
        self.anchor(c.position);
        self.u16(c.block);
        self.u16(c.modes);
        self.u8(c.color);
    }
    fn endpoints(&mut self, endpoints: &[Endpoint]) {
        self.count(endpoints.len());
        for e in endpoints {
            self.u64(e.owner);
            self.position(e.position);
            self.u8(e.face as u8);
            self.u8(u8::from(e.pull));
        }
    }
    fn network_meta(&mut self, m: &NetworkMeta) {
        self.key(m.key);
        self.u64(m.order);
        self.count(m.producers.len());
        for id in &m.producers {
            self.u64(id.0);
        }
    }
    fn edit(&mut self, e: TopologyEdit) {
        match e {
            TopologyEdit::Add {
                position,
                resource,
                links,
            } => {
                self.u8(0);
                self.position(position);
                self.resource(resource);
                self.u8(links);
            }
            TopologyEdit::Remove { position } => {
                self.u8(1);
                self.position(position);
            }
            TopologyEdit::Relink { position, links } => {
                self.u8(2);
                self.position(position);
                self.u8(links);
            }
        }
    }
    fn indexed_slots(&mut self, slots: &[IndexedSlot]) {
        self.count(slots.len());
        for s in slots {
            self.u32(s.index);
            self.u8(u8::from(s.state.is_some()));
            if let Some(state) = &s.state {
                self.slot(state);
            }
        }
    }
    fn nodes(&mut self, nodes: &[NodeSnapshot]) {
        self.count(nodes.len());
        for n in nodes {
            self.node(n);
        }
    }
    fn topology(&mut self, t: &TopologyRecords) {
        self.edit(t.edit);
        self.count(t.extra_edits.len());
        for &edit in &t.extra_edits {
            self.edit(edit);
        }
        self.u32(t.slots_len_before);
        self.u32(t.slots_len_after);
        self.nodes(&t.before_nodes);
        self.nodes(&t.after_nodes);
        self.indexed_slots(&t.before_slots);
        self.indexed_slots(&t.after_slots);
        self.u64(t.overflow);
    }
}
impl Reader<'_> {
    fn peek(&self, magic: &[u8; 4]) -> bool {
        self.bytes.get(self.cursor..self.cursor.saturating_add(4)) == Some(magic.as_slice())
    }
    fn transmitter(&mut self) -> Result<TransmitterConfig, MachineError> {
        let value = TransmitterConfig {
            position: self.anchor()?,
            block: self.u16()?,
            modes: self.u16()?,
            color: self.u8()?,
        };
        validate_config(value)?;
        Ok(value)
    }
    fn endpoints(&mut self) -> Result<Vec<Endpoint>, MachineError> {
        let count = self.count(self.config.endpoints, 22)?;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(Endpoint {
                owner: self.u64()?,
                position: self.position()?,
                face: *Face::ALL
                    .get(self.u8()? as usize)
                    .ok_or(MachineError::Encoding)?,
                pull: self.boolean()?,
            });
        }
        Ok(out)
    }
    fn network_meta(&mut self) -> Result<NetworkMeta, MachineError> {
        let key = self.key()?;
        let order = self.u64()?;
        let count = self.count(self.config.machines, 8)?;
        let mut producers = Vec::with_capacity(count);
        for _ in 0..count {
            producers.push(ComponentId(self.u64()?));
        }
        Ok(NetworkMeta {
            key,
            order,
            producers,
        })
    }
    fn edit(&mut self) -> Result<TopologyEdit, MachineError> {
        let kind = self.u8()?;
        let position = self.position()?;
        match kind {
            0 => Ok(TopologyEdit::Add {
                position,
                resource: self.resource()?,
                links: self.u8()?,
            }),
            1 => Ok(TopologyEdit::Remove { position }),
            2 => Ok(TopologyEdit::Relink {
                position,
                links: self.u8()?,
            }),
            _ => Err(MachineError::Encoding),
        }
    }
    fn nodes(&mut self) -> Result<Vec<NodeSnapshot>, MachineError> {
        let count = self.count(self.config.transmitters, 22)?;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(self.node()?);
        }
        Ok(out)
    }
    fn indexed_slots(&mut self) -> Result<Vec<IndexedSlot>, MachineError> {
        let count = self.count(self.config.transmitters, 5)?;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            let index = self.u32()?;
            if index as usize >= self.config.transmitters {
                return Err(MachineError::Bounds);
            }
            let state = if self.boolean()? {
                Some(self.slot()?)
            } else {
                None
            };
            out.push(IndexedSlot { index, state });
        }
        Ok(out)
    }
    fn topology(&mut self) -> Result<TopologyRecords, MachineError> {
        let edit = self.edit()?;
        let count = self.count(63, 13)?;
        let mut extra_edits = Vec::with_capacity(count);
        for _ in 0..count {
            extra_edits.push(self.edit()?);
        }
        let slots_len_before = self.u32()?;
        let slots_len_after = self.u32()?;
        if slots_len_before as usize > self.config.transmitters
            || slots_len_after as usize > self.config.transmitters
        {
            return Err(MachineError::Bounds);
        }
        Ok(TopologyRecords {
            edit,
            extra_edits,
            slots_len_before,
            slots_len_after,
            before_nodes: self.nodes()?,
            after_nodes: self.nodes()?,
            before_slots: self.indexed_slots()?,
            after_slots: self.indexed_slots()?,
            overflow: self.u64()?,
        })
    }
}
