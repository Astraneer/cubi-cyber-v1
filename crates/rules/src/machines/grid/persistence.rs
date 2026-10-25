//! Exact owner snapshots and detached reservoir projection. Topology arrays are
//! copied only for checkpoints; a hot phase copies six scalar reservoir fields.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reservoir {
    pub substance: u8,
    pub stored: u64,
    pub glow: u16,
    pub flow: u64,
    pub sent: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeSnapshot {
    pub position: Position,
    pub network: NetworkId,
    pub resource: Resource,
    pub links: u8,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkSnapshot {
    pub resource: Resource,
    pub reservoir: Reservoir,
    pub members: Vec<Position>,
    pub endpoints: Vec<Endpoint>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotSnapshot {
    pub generation: u32,
    pub network: Option<NetworkSnapshot>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridSnapshot {
    pub nodes: Vec<NodeSnapshot>,
    pub slots: Vec<SlotSnapshot>,
}
pub struct PhaseScratch {
    requests: Vec<u64>,
    grants: Vec<u64>,
}
impl PhaseScratch {
    pub fn new(maximum_endpoints: usize) -> Self {
        Self {
            requests: Vec::with_capacity(maximum_endpoints),
            grants: Vec::with_capacity(maximum_endpoints),
        }
    }
}
impl Network {
    pub fn reservoir(&self) -> Reservoir {
        Reservoir {
            substance: self.substance,
            stored: self.stored,
            glow: self.glow,
            flow: self.flow,
            sent: self.sent,
        }
    }
    fn valid_reservoir(&self, state: Reservoir) -> bool {
        state.stored <= self.capacity()
            && state.glow <= 1000
            && match self.resource {
                Resource::Energy => state.substance == 0,
                Resource::Fluid => {
                    (state.stored == 0 && state.substance == 0) || [3, 6].contains(&state.substance)
                }
                Resource::Gas => {
                    (state.stored == 0 && state.substance == 0) || [4, 5].contains(&state.substance)
                }
            }
    }
}
impl Grid {
    /// One indexed member; owner edits never need a complete grid snapshot.
    pub fn node(&self, position: Position) -> Option<NodeSnapshot> {
        self.members.get(&position).map(|member| NodeSnapshot {
            position,
            network: member.network,
            resource: member.resource,
            links: member.links,
        })
    }
    pub fn validate_reservoir(&self, id: NetworkId, state: Reservoir) -> Result<(), GridError> {
        if self.network(id)?.valid_reservoir(state) {
            Ok(())
        } else {
            Err(GridError::InvalidSubstance)
        }
    }
    pub fn network_ids(&self) -> impl Iterator<Item = NetworkId> + '_ {
        self.slots.iter().enumerate().filter_map(|(slot, state)| {
            state.network.as_ref().map(|_| NetworkId {
                slot: slot as u32,
                generation: state.generation,
            })
        })
    }
    pub fn snapshot(&self) -> GridSnapshot {
        GridSnapshot {
            nodes: self
                .members
                .iter()
                .map(|(&position, member)| NodeSnapshot {
                    position,
                    network: member.network,
                    resource: member.resource,
                    links: member.links,
                })
                .collect(),
            slots: self
                .slots
                .iter()
                .map(|slot| SlotSnapshot {
                    generation: slot.generation,
                    network: slot.network.as_ref().map(|n| NetworkSnapshot {
                        resource: n.resource,
                        reservoir: n.reservoir(),
                        members: n.members.clone(),
                        endpoints: n.endpoints.clone(),
                    }),
                })
                .collect(),
        }
    }
    pub fn restore(
        max_nodes: usize,
        max_endpoints: usize,
        state: &GridSnapshot,
    ) -> Result<Self, GridError> {
        if state.nodes.len() > max_nodes
            || state.slots.len() > max_nodes
            || state
                .nodes
                .windows(2)
                .any(|p| p[0].position >= p[1].position)
        {
            return Err(GridError::Capacity);
        }
        let mut grid = Self::new(max_nodes, max_endpoints);
        let mut seen = BTreeSet::new();
        for (index, slot) in state.slots.iter().enumerate() {
            if slot.generation == 0 {
                return Err(GridError::Stale);
            }
            let network = if let Some(n) = &slot.network {
                if n.members.is_empty()
                    || n.members.len() > max_nodes
                    || n.endpoints.len() > max_endpoints
                    || n.endpoints.windows(2).any(|pair| {
                        (
                            pair[0].owner,
                            pair[0].pull,
                            pair[0].position,
                            pair[0].face as u8,
                        ) >= (
                            pair[1].owner,
                            pair[1].pull,
                            pair[1].position,
                            pair[1].face as u8,
                        ) || (pair[0].owner, pair[0].pull) == (pair[1].owner, pair[1].pull)
                    })
                {
                    return Err(GridError::InvalidEndpoints);
                }
                let id = NetworkId {
                    slot: index as u32,
                    generation: slot.generation,
                };
                for &p in &n.members {
                    if !seen.insert(p)
                        || state
                            .nodes
                            .binary_search_by_key(&p, |node| node.position)
                            .ok()
                            .and_then(|index| state.nodes.get(index))
                            .is_none_or(|node| {
                                node.network != id
                                    || node.resource != n.resource
                                    || node.links & !63 != 0
                            })
                    {
                        return Err(GridError::Missing);
                    }
                }
                let value = Network {
                    resource: n.resource,
                    substance: n.reservoir.substance,
                    stored: n.reservoir.stored,
                    glow: n.reservoir.glow,
                    flow: n.reservoir.flow,
                    sent: n.reservoir.sent,
                    members: n.members.clone(),
                    endpoints: n.endpoints.clone(),
                    requests: vec![0; n.endpoints.len()],
                    grants: vec![0; n.endpoints.len()],
                };
                if !value.valid_reservoir(n.reservoir) {
                    return Err(GridError::InvalidSubstance);
                }
                Some(value)
            } else {
                None
            };
            grid.slots.push(Slot {
                generation: slot.generation,
                network,
            });
            if grid.slots[index].network.is_none() && slot.generation < u32::MAX {
                grid.free_slots.insert(index as u32);
            }
        }
        if seen.len() != state.nodes.len() {
            return Err(GridError::Missing);
        }
        for node in &state.nodes {
            grid.members.insert(
                node.position,
                Member {
                    network: node.network,
                    resource: node.resource,
                    links: node.links,
                },
            );
        }
        Ok(grid)
    }
    pub fn apply_reservoir(
        &mut self,
        id: NetworkId,
        before: Reservoir,
        after: Reservoir,
    ) -> Result<(), GridError> {
        let network = self.network_mut(id)?;
        if network.reservoir() != before {
            return Err(GridError::Stale);
        }
        if !network.valid_reservoir(after) {
            return Err(GridError::InvalidSubstance);
        }
        network.substance = after.substance;
        network.stored = after.stored;
        network.glow = after.glow;
        network.flow = after.flow;
        network.sent = after.sent;
        Ok(())
    }
    /// `before` can include earlier detached producer inserts in this tick.
    /// Callback writes belong to the caller's bounded transaction overlay.
    pub fn project_phase(
        &self,
        id: NetworkId,
        mut state: Reservoir,
        w: &mut impl GridWorld,
        work: &mut GridWork,
        scratch: &mut PhaseScratch,
    ) -> Result<(Reservoir, bool), GridError> {
        let n = self.network(id)?;
        if !n.valid_reservoir(state) {
            return Err(GridError::InvalidSubstance);
        }
        if n.endpoints.len() > scratch.requests.capacity()
            || n.endpoints.len() > scratch.grants.capacity()
        {
            return Err(GridError::Capacity);
        }
        scratch.requests.resize(n.endpoints.len(), 0);
        scratch.grants.resize(n.endpoints.len(), 0);
        work.visited_networks += 1;
        let mut pulled = 0;
        if n.resource != Resource::Energy {
            for &endpoint in &n.endpoints {
                if !endpoint.pull || !w.active(endpoint) {
                    continue;
                }
                let free = n.capacity().saturating_sub(state.stored);
                if free == 0 {
                    break;
                }
                let (sub, amount) = w.extract(
                    endpoint,
                    n.resource,
                    state.substance,
                    n.resource.pull_limit().min(free),
                );
                let valid = match n.resource {
                    Resource::Fluid => [3, 6].contains(&sub),
                    Resource::Gas => [4, 5].contains(&sub),
                    Resource::Energy => false,
                };
                if valid && (state.substance == 0 || sub == state.substance) {
                    let accepted = amount.min(free).min(n.resource.pull_limit());
                    state.stored += accepted;
                    pulled += accepted;
                    if accepted > 0 {
                        state.substance = sub;
                    }
                }
            }
        }
        let fill = (u128::from(state.stored.min(n.capacity())) * 1000
            / u128::from(n.capacity().max(1))) as u16;
        let before_glow = state.glow;
        state.glow = state.glow.max(fill);
        state.glow = if state.sent {
            (state.glow + 20).min(1000)
        } else {
            state.glow.saturating_sub(20).max(fill)
        };
        for (i, &endpoint) in n.endpoints.iter().enumerate() {
            scratch.requests[i] = if !endpoint.pull && w.active(endpoint) && state.stored > 0 {
                work.endpoint_queries += 1;
                w.room(endpoint, n.resource, state.substance)
            } else {
                0
            };
        }
        fair_share(state.stored, &scratch.requests, &mut scratch.grants)
            .map_err(|_| GridError::InvalidEndpoints)?;
        let mut sent = 0;
        for (i, &endpoint) in n.endpoints.iter().enumerate() {
            if scratch.grants[i] > 0 {
                work.transfers += 1;
                sent += w
                    .give(endpoint, n.resource, state.substance, scratch.grants[i])
                    .min(scratch.grants[i]);
            }
        }
        state.stored -= sent;
        state.flow = sent;
        state.sent = sent > 0;
        if n.resource != Resource::Energy && state.stored == 0 && !state.sent {
            state.substance = 0;
        }
        Ok((state, sent > 0 || pulled > 0 || before_glow != state.glow))
    }
}
