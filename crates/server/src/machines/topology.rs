//! World connection derivation and guarded topology/cache publication.
use super::*;
use machines::grid::{Endpoint, Network, NetworkSnapshot, TopologyBudget, TopologyDelta};

pub(super) enum NetworkView<'a> {
    Stored(&'a Network),
    Planned(&'a NetworkSnapshot),
}
impl NetworkView<'_> {
    pub(super) fn resource(&self) -> Resource {
        match self {
            Self::Stored(n) => n.resource,
            Self::Planned(n) => n.resource,
        }
    }
    pub(super) fn members(&self) -> &[[i32; 3]] {
        match self {
            Self::Stored(n) => n.members(),
            Self::Planned(n) => &n.members,
        }
    }
    pub(super) fn endpoints(&self) -> &[Endpoint] {
        match self {
            Self::Stored(n) => n.endpoints(),
            Self::Planned(n) => &n.endpoints,
        }
    }
}
impl MachineRuntime {
    pub fn transmitter_config(&self, p: BlockPos) -> Option<TransmitterConfig> {
        self.configs.get(&p).copied()
    }
    pub fn transmitter_configs_in_page(
        &self,
        page: PageId,
    ) -> impl Iterator<Item = &TransmitterConfig> {
        self.config_pages
            .get(&page)
            .into_iter()
            .flat_map(|positions| positions.iter())
            .filter_map(|p| self.configs.get(p))
    }
    pub fn network_at(&self, p: BlockPos) -> Option<NetworkKey> {
        self.grids
            .get(&p.space)?
            .network_at([p.x, p.y, p.z])
            .map(|network| NetworkKey {
                space: p.space,
                network,
            })
    }
    pub fn reservoir_at(&self, p: BlockPos) -> Option<Reservoir> {
        let key = self.network_at(p)?;
        self.grids
            .get(&p.space)?
            .network(key.network)
            .ok()
            .map(Network::reservoir)
    }
    pub(super) fn network_meta(&self, key: NetworkKey) -> Option<NetworkMeta> {
        self.network_orders.get(&key).map(|&order| NetworkMeta {
            key,
            order,
            producers: self.producers.get(&key).cloned().unwrap_or_default(),
        })
    }
    pub(super) fn projected_network<'a>(
        &'a self,
        key: NetworkKey,
        delta: &'a RuntimeDelta,
    ) -> Result<NetworkView<'a>, MachineError> {
        if let Some(change) = delta
            .topology
            .iter()
            .find(|change| change.space == key.space)
            && let Ok(index) = change
                .records
                .after_slots
                .binary_search_by_key(&key.network.slot, |slot| slot.index)
        {
            let slot = change.records.after_slots[index]
                .state
                .as_ref()
                .ok_or(MachineError::Missing)?;
            if slot.generation != key.network.generation {
                return Err(MachineError::Stale);
            }
            return Ok(NetworkView::Planned(
                slot.network.as_ref().ok_or(MachineError::Missing)?,
            ));
        }
        Ok(NetworkView::Stored(
            self.grids
                .get(&key.space)
                .ok_or(MachineError::Missing)?
                .network(key.network)?,
        ))
    }
    fn topology_budget(&self) -> TopologyBudget {
        TopologyBudget {
            nodes: (self.config.transmitters as u64)
                .saturating_mul(4)
                .saturating_add(512),
            endpoints: (save::MAX_RUNTIME_BYTES as u64).saturating_mul(2),
            bytes: (save::MAX_RUNTIME_BYTES as u64).saturating_mul(4),
        }
    }
    pub(super) fn prepare_topology(
        &self,
        delta: &RuntimeDelta,
    ) -> Result<Vec<(SpaceId, TopologyDelta)>, MachineError> {
        if delta.topology.len() > 64 || !delta.topology.is_empty() && !delta.reservoirs.is_empty() {
            return Err(MachineError::Encoding);
        }
        let mut spaces = BTreeSet::new();
        let mut total_nodes = self.grids.values().map(Grid::node_count).sum::<usize>();
        let mut total_spaces = self.grids.len();
        let mut out = Vec::with_capacity(delta.topology.len());
        for patch in &delta.topology {
            if !spaces.insert(patch.space) {
                return Err(MachineError::Encoding);
            }
            total_nodes = total_nodes
                .checked_sub(patch.records.before_nodes.len())
                .and_then(|n| n.checked_add(patch.records.after_nodes.len()))
                .ok_or(MachineError::Encoding)?;
            total_spaces += usize::from(!self.grids.contains_key(&patch.space));
            if total_nodes > self.config.transmitters || total_spaces > 64 {
                return Err(MachineError::Bounds);
            }
            let empty = Grid::new(self.config.transmitters, self.config.endpoints);
            let grid = self.grids.get(&patch.space).unwrap_or(&empty);
            out.push((
                patch.space,
                grid.restore_topology_delta(
                    patch.records.clone(),
                    self.topology_budget(),
                    &mut GridWork::default(),
                )?,
            ));
        }
        Ok(out)
    }
    pub(super) fn validate_extensions(&self, delta: &RuntimeDelta) -> Result<(), MachineError> {
        if delta.topology.is_empty()
            && delta.networks.is_empty()
            && delta.endpoints.is_empty()
            && delta.configs.is_empty()
        {
            return Ok(());
        }
        self.prepare_topology(delta)?;
        let meta_after: BTreeMap<_, _> = delta
            .machines
            .iter()
            .filter_map(|p| p.after.or(p.before).map(|m| (m.id, p.after)))
            .collect();
        let exists = |id: ComponentId| {
            meta_after
                .get(&id)
                .copied()
                .unwrap_or_else(|| self.machine_meta(id))
        };
        let mut networks = BTreeSet::new();
        let mut orders = BTreeSet::new();
        for patch in &delta.networks {
            let key = patch
                .after
                .as_ref()
                .or(patch.before.as_ref())
                .ok_or(MachineError::Encoding)?
                .key;
            if !networks.insert(key)
                || self.network_meta(key) != patch.before
                || patch
                    .before
                    .as_ref()
                    .zip(patch.after.as_ref())
                    .is_some_and(|(a, b)| a.key != b.key || a.order != b.order)
            {
                return Err(MachineError::Stale);
            }
            if let Some(after) = &patch.after {
                self.projected_network(key, delta)?;
                if after.order >= delta.next_order_after
                    || patch.before.is_none() && after.order < self.next_order
                    || !orders.insert(after.order)
                    || after.producers.len() > self.config.machines
                    || after.producers.windows(2).any(|p| p[0] >= p[1])
                    || after.producers.iter().any(|id| exists(*id).is_none())
                {
                    return Err(MachineError::Encoding);
                }
            } else if self.projected_network(key, delta).is_ok() {
                return Err(MachineError::Encoding);
            }
        }
        let metadata: BTreeMap<_, _> = delta
            .networks
            .iter()
            .filter_map(|p| {
                p.after
                    .as_ref()
                    .or(p.before.as_ref())
                    .map(|m| (m.key, p.after.as_ref()))
            })
            .collect();
        for patch in &delta.topology {
            for slot in &patch.records.after_slots {
                let Some(state) = &slot.state else { continue };
                if state.network.is_none() {
                    continue;
                }
                let key = NetworkKey {
                    space: patch.space,
                    network: NetworkId {
                        slot: slot.index,
                        generation: state.generation,
                    },
                };
                if metadata
                    .get(&key)
                    .map(|m| m.is_none())
                    .unwrap_or_else(|| !self.network_orders.contains_key(&key))
                {
                    return Err(MachineError::Encoding);
                }
            }
            for slot in &patch.records.before_slots {
                let Some(state) = &slot.state else { continue };
                if state.network.is_none() {
                    continue;
                }
                let key = NetworkKey {
                    space: patch.space,
                    network: NetworkId {
                        slot: slot.index,
                        generation: state.generation,
                    },
                };
                if self.projected_network(key, delta).is_err() {
                    if metadata.get(&key) != Some(&None) {
                        return Err(MachineError::Encoding);
                    }
                    if self.pending(Event::Network(key)).is_some()
                        && !delta.queue.iter().any(|p| {
                            p.before.is_some_and(|p| p.event == Event::Network(key))
                                && p.after.is_none()
                        })
                    {
                        return Err(MachineError::Encoding);
                    }
                }
            }
        }
        let mut endpoints = BTreeSet::new();
        for patch in &delta.endpoints {
            let network = self.projected_network(patch.key, delta)?;
            if !endpoints.insert(patch.key)
                || network.endpoints() != patch.before
                || patch.after.len() > self.config.endpoints
                || patch.after.windows(2).any(|p| {
                    (p[0].owner, p[0].pull, p[0].position, p[0].face as u8)
                        >= (p[1].owner, p[1].pull, p[1].position, p[1].face as u8)
                        || (p[0].owner, p[0].pull) == (p[1].owner, p[1].pull)
                })
            {
                return Err(MachineError::Encoding);
            }
            let members: BTreeSet<_> = network.members().iter().copied().collect();
            for endpoint in &patch.after {
                let meta = exists(ComponentId(endpoint.owner)).ok_or(MachineError::Missing)?;
                if meta.anchor.space != patch.key.space
                    || [meta.anchor.x, meta.anchor.y, meta.anchor.z] != endpoint.position
                    || sides::offset(endpoint.position, endpoint.face)
                        .is_none_or(|p| !members.contains(&p))
                {
                    return Err(MachineError::Encoding);
                }
            }
        }
        let mut configs = BTreeSet::new();
        let mut count = self.configs.len();
        for patch in &delta.configs {
            let p = patch
                .after
                .or(patch.before)
                .ok_or(MachineError::Encoding)?
                .position;
            if !configs.insert(p)
                || self.transmitter_config(p) != patch.before
                || patch
                    .before
                    .zip(patch.after)
                    .is_some_and(|(a, b)| a.position != b.position)
            {
                return Err(MachineError::Stale);
            }
            if let Some(after) = patch.after {
                validate_config(after)?;
            }
            count = count
                .saturating_sub(usize::from(patch.before.is_some()))
                .saturating_add(usize::from(patch.after.is_some()));
        }
        if count > self.config.transmitters {
            return Err(MachineError::Bounds);
        }
        Ok(())
    }
    pub(super) fn apply_extensions(
        &mut self,
        delta: &RuntimeDelta,
        prepared: Vec<(SpaceId, TopologyDelta)>,
    ) -> Result<(), MachineError> {
        for (space, change) in prepared {
            self.grids
                .entry(space)
                .or_insert_with(|| Grid::new(self.config.transmitters, self.config.endpoints))
                .apply_edit(change)?;
        }
        for patch in &delta.endpoints {
            self.grids
                .get_mut(&patch.key.space)
                .ok_or(MachineError::Missing)?
                .set_endpoints(patch.key.network, &patch.after)?;
        }
        for patch in &delta.networks {
            if let Some(before) = &patch.before {
                self.network_orders.remove(&before.key);
                self.producers.remove(&before.key);
            }
            if let Some(after) = &patch.after {
                self.network_orders.insert(after.key, after.order);
                self.producers.insert(after.key, after.producers.clone());
            }
        }
        for patch in &delta.configs {
            if let Some(before) = patch.before {
                self.configs.remove(&before.position);
                if patch.after.is_none()
                    && let Some(positions) = self.config_pages.get_mut(&before.position.page())
                {
                    positions.remove(&before.position);
                    if positions.is_empty() {
                        self.config_pages.remove(&before.position.page());
                    }
                }
            }
            if let Some(after) = patch.after {
                self.configs.insert(after.position, after);
                if patch.before.is_none() {
                    self.config_pages
                        .entry(after.position.page())
                        .or_default()
                        .insert(after.position);
                }
            }
        }
        Ok(())
    }
}
