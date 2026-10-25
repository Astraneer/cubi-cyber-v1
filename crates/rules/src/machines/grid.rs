//! Incremental connected resource networks. A block edit visits its neighboring
//! components only; unrelated networks are never rebuilt. Allocation is confined
//! to topology/endpoint edits, with fixed configured node/endpoint bounds.
mod persistence;
use super::{
    logistics::fair_share,
    sides::{self, Face},
};
pub use persistence::{
    GridSnapshot, NetworkSnapshot, NodeSnapshot, PhaseScratch, Reservoir, SlotSnapshot,
};
use std::collections::{BTreeMap, BTreeSet};

pub type Position = [i32; 3];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Energy,
    Fluid,
    Gas,
}
impl Resource {
    pub fn unit_capacity(self) -> u64 {
        match self {
            Self::Energy => 8000,
            Self::Fluid => 2000,
            Self::Gas => 4000,
        }
    }
    pub fn pull_limit(self) -> u64 {
        match self {
            Self::Energy => 0,
            Self::Fluid => 250,
            Self::Gas => 750,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NetworkId {
    pub slot: u32,
    pub generation: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Member {
    network: NetworkId,
    resource: Resource,
    links: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Endpoint {
    pub owner: u64,
    pub position: Position,
    pub face: Face,
    pub pull: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GridWork {
    pub topology_nodes: u64,
    pub endpoint_queries: u64,
    pub transfers: u64,
    pub visited_networks: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridError {
    Capacity,
    Exists,
    Missing,
    Stale,
    InvalidSubstance,
    InvalidEndpoints,
}
#[derive(Clone, Debug)]
pub struct Network {
    pub resource: Resource,
    pub substance: u8,
    pub stored: u64,
    pub glow: u16,
    pub flow: u64,
    members: Vec<Position>,
    endpoints: Vec<Endpoint>,
    requests: Vec<u64>,
    grants: Vec<u64>,
    sent: bool,
}
impl Network {
    pub fn members(&self) -> &[Position] {
        &self.members
    }
    pub fn capacity(&self) -> u64 {
        self.members.len() as u64 * self.resource.unit_capacity()
    }
    pub fn endpoints(&self) -> &[Endpoint] {
        &self.endpoints
    }
}
#[derive(Clone, Debug)]
struct Slot {
    generation: u32,
    network: Option<Network>,
}

/// Owner callbacks operate on a detached transaction plan. An endpoint's stable
/// owner ID prevents double energy delivery when several faces touch one bus.
pub trait GridWorld {
    fn active(&self, _endpoint: Endpoint) -> bool {
        true
    }
    fn room(&mut self, endpoint: Endpoint, resource: Resource, substance: u8) -> u64;
    fn give(&mut self, endpoint: Endpoint, resource: Resource, substance: u8, amount: u64) -> u64;
    fn extract(
        &mut self,
        _endpoint: Endpoint,
        _resource: Resource,
        _wanted: u8,
        _amount: u64,
    ) -> (u8, u64) {
        (0, 0)
    }
}

pub struct Grid {
    members: BTreeMap<Position, Member>,
    slots: Vec<Slot>,
    max_nodes: usize,
    max_endpoints: usize,
}
impl Grid {
    pub fn new(max_nodes: usize, max_endpoints: usize) -> Self {
        Self {
            members: BTreeMap::new(),
            slots: Vec::new(),
            max_nodes,
            max_endpoints,
        }
    }
    pub fn node_count(&self) -> usize {
        self.members.len()
    }
    pub fn network_at(&self, p: Position) -> Option<NetworkId> {
        self.members.get(&p).map(|m| m.network)
    }
    pub fn network(&self, id: NetworkId) -> Result<&Network, GridError> {
        let slot = self.slots.get(id.slot as usize).ok_or(GridError::Stale)?;
        if slot.generation != id.generation {
            return Err(GridError::Stale);
        }
        slot.network.as_ref().ok_or(GridError::Stale)
    }
    fn network_mut(&mut self, id: NetworkId) -> Result<&mut Network, GridError> {
        let slot = self
            .slots
            .get_mut(id.slot as usize)
            .ok_or(GridError::Stale)?;
        if slot.generation != id.generation {
            return Err(GridError::Stale);
        }
        slot.network.as_mut().ok_or(GridError::Stale)
    }
    fn allocate(&mut self, resource: Resource) -> Result<NetworkId, GridError> {
        let available = self
            .slots
            .iter()
            .position(|slot| slot.network.is_none() && slot.generation < u32::MAX);
        let slot = if let Some(i) = available {
            i
        } else {
            if self.slots.len() == self.max_nodes {
                return Err(GridError::Capacity);
            }
            self.slots.push(Slot {
                generation: 0,
                network: None,
            });
            self.slots.len() - 1
        };
        self.slots[slot].generation += 1;
        let id = NetworkId {
            slot: slot as u32,
            generation: self.slots[slot].generation,
        };
        self.slots[slot].network = Some(Network {
            resource,
            substance: 0,
            stored: 0,
            glow: 0,
            flow: 0,
            members: Vec::new(),
            endpoints: Vec::new(),
            requests: Vec::new(),
            grants: Vec::new(),
            sent: false,
        });
        Ok(id)
    }
    fn connected(&self, a: Position, b: Position, face: Face) -> bool {
        let Some(ma) = self.members.get(&a) else {
            return false;
        };
        let Some(mb) = self.members.get(&b) else {
            return false;
        };
        ma.resource == mb.resource
            && ma.links & (1 << face as u8) != 0
            && mb.links & (1 << face.opposite() as u8) != 0
    }
    pub fn add(
        &mut self,
        p: Position,
        resource: Resource,
        links: u8,
        work: &mut GridWork,
    ) -> Result<NetworkId, GridError> {
        if self.members.contains_key(&p) {
            return Err(GridError::Exists);
        }
        if self.members.len() == self.max_nodes || links & !63 != 0 {
            return Err(GridError::Capacity);
        }
        let id = self.allocate(resource)?;
        self.members.insert(
            p,
            Member {
                network: id,
                resource,
                links,
            },
        );
        self.network_mut(id)?.members.push(p);
        work.topology_nodes += 1;
        self.reconcile(p, work)
    }
    /// Reconsider the six links after a reservoir becomes empty or its neighbor
    /// is inserted. Different nonempty fluid/gas substances remain separate.
    pub fn reconcile(&mut self, p: Position, work: &mut GridWork) -> Result<NetworkId, GridError> {
        let own = self.network_at(p).ok_or(GridError::Missing)?;
        let mut candidates = [None; 7];
        candidates[0] = Some(own);
        let mut count = 1;
        for face in Face::ALL {
            let Some(q) = sides::offset(p, face) else {
                continue;
            };
            if !self.connected(p, q, face) {
                continue;
            }
            let id = self.network_at(q).expect("connected member");
            if !candidates[..count].contains(&Some(id)) {
                candidates[count] = Some(id);
                count += 1;
            }
        }
        let mut ids: Vec<NetworkId> = candidates[..count].iter().flatten().copied().collect();
        ids.sort_by(|a, b| {
            self.network(*b)
                .expect("live")
                .members
                .len()
                .cmp(&self.network(*a).expect("live").members.len())
                .then(a.cmp(b))
        });
        let mut keep = own;
        for id in ids {
            if id == keep || self.network(id).is_err() {
                continue;
            }
            let a = self.network(keep)?;
            let b = self.network(id)?;
            if a.substance != 0 && b.substance != 0 && a.substance != b.substance {
                continue;
            }
            let (target, source) = if a.members.len() > b.members.len()
                || (a.members.len() == b.members.len() && keep < id)
            {
                (keep, id)
            } else {
                (id, keep)
            };
            self.merge(target, source, work)?;
            keep = target;
        }
        Ok(keep)
    }
    fn merge(
        &mut self,
        target: NetworkId,
        source: NetworkId,
        work: &mut GridWork,
    ) -> Result<(), GridError> {
        let other = self.slots[source.slot as usize]
            .network
            .take()
            .ok_or(GridError::Stale)?;
        for p in &other.members {
            self.members.get_mut(p).expect("member").network = target;
        }
        work.topology_nodes += other.members.len() as u64;
        let n = self.network_mut(target)?;
        let old_cap = n.capacity();
        let other_cap = other.capacity();
        n.glow = ((u128::from(n.glow) * u128::from(old_cap)
            + u128::from(other.glow) * u128::from(other_cap))
            / u128::from(old_cap + other_cap)) as u16;
        n.stored += other.stored;
        if n.substance == 0 {
            n.substance = other.substance;
        }
        n.members.extend(other.members);
        n.members.sort_unstable();
        // Endpoint membership is invalid after topology changes; the owner
        // rebuilds the affected boundary cache before the next grid phase.
        n.endpoints.clear();
        n.requests.clear();
        n.grants.clear();
        Ok(())
    }
    /// Returns reservoir overflow caused by deleting capacity. It must be
    /// recorded explicitly by the owner; deletion never silently duplicates it.
    pub fn remove(&mut self, p: Position, work: &mut GridWork) -> Result<u64, GridError> {
        let member = self.members.remove(&p).ok_or(GridError::Missing)?;
        let id = member.network;
        let old = self.slots[id.slot as usize]
            .network
            .take()
            .ok_or(GridError::Stale)?;
        let remaining: Vec<_> = old.members.iter().copied().filter(|q| *q != p).collect();
        if remaining.is_empty() {
            return Ok(old.stored);
        }
        let mut seen = BTreeSet::new();
        let mut parts = Vec::<Vec<Position>>::new();
        for &seed in &remaining {
            if !seen.insert(seed) {
                continue;
            }
            let mut part = vec![seed];
            let mut head = 0;
            while head < part.len() {
                let node = part[head];
                head += 1;
                work.topology_nodes += 1;
                for face in Face::ALL {
                    let Some(q) = sides::offset(node, face) else {
                        continue;
                    };
                    if self.connected(node, q, face)
                        && self.members.get(&q).is_some_and(|m| m.network == id)
                        && seen.insert(q)
                    {
                        part.push(q);
                    }
                }
            }
            part.sort_unstable();
            parts.push(part);
        }
        parts.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));
        let capacity = remaining.len() as u64 * old.resource.unit_capacity();
        let stored = old.stored.min(capacity);
        let lost = old.stored - stored;
        let mut assigned = 0;
        // Largest part retains both handle and remainder. Reserve that slot
        // while allocating the others so no ABA reuse can steal its identity.
        let mut kept = old.clone();
        kept.members = parts[0].clone();
        kept.stored = 0;
        kept.endpoints.clear();
        kept.requests.clear();
        kept.grants.clear();
        self.slots[id.slot as usize].network = Some(kept);
        for part in parts.into_iter().skip(1) {
            let new = self.allocate(old.resource)?;
            let share =
                (u128::from(stored) * part.len() as u128 * old.resource.unit_capacity() as u128
                    / u128::from(capacity)) as u64;
            assigned += share;
            for q in &part {
                self.members.get_mut(q).expect("member").network = new;
            }
            let n = self.network_mut(new)?;
            n.members = part;
            n.stored = share;
            n.substance = if share > 0 { old.substance } else { 0 };
            n.glow = old.glow;
            n.sent = old.sent;
        }
        let n = self.network_mut(id)?;
        n.stored = stored - assigned;
        n.substance = if n.stored > 0 { old.substance } else { 0 };
        Ok(lost)
    }
    pub fn insert(&mut self, p: Position, substance: u8, amount: u64) -> Result<u64, GridError> {
        let id = self.network_at(p).ok_or(GridError::Missing)?;
        let n = self.network_mut(id)?;
        let allowed = match n.resource {
            Resource::Energy => substance == 0,
            Resource::Fluid => [3, 6].contains(&substance),
            Resource::Gas => [4, 5].contains(&substance),
        };
        if !allowed {
            return Err(GridError::InvalidSubstance);
        }
        if n.substance != 0 && n.substance != substance {
            return Ok(0);
        }
        let accepted = amount.min(n.capacity().saturating_sub(n.stored));
        if accepted > 0 {
            n.stored += accepted;
            n.substance = substance;
        }
        Ok(accepted)
    }
    pub fn set_endpoints(
        &mut self,
        id: NetworkId,
        endpoints: &[Endpoint],
    ) -> Result<(), GridError> {
        if endpoints.len() > self.max_endpoints {
            return Err(GridError::Capacity);
        }
        let mut cache = endpoints.to_vec();
        cache.sort_unstable_by_key(|e| (e.owner, e.pull, e.position, e.face as u8));
        cache.dedup_by_key(|e| (e.owner, e.pull));
        let n = self.network_mut(id)?;
        n.endpoints = cache;
        n.requests = vec![0; n.endpoints.len()];
        n.grants = vec![0; n.endpoints.len()];
        Ok(())
    }
    /// One previously scheduled network, without a registry scan. Callers wake
    /// a reservoir on input/endpoint changes or until its glow settles.
    pub fn phase(
        &mut self,
        id: NetworkId,
        w: &mut impl GridWorld,
        work: &mut GridWork,
    ) -> Result<bool, GridError> {
        let n = self.network_mut(id)?;
        work.visited_networks += 1;
        let mut pulled = 0;
        if n.resource != Resource::Energy {
            for &endpoint in &n.endpoints {
                if !endpoint.pull || !w.active(endpoint) {
                    continue;
                }
                let free = n.capacity().saturating_sub(n.stored);
                if free == 0 {
                    break;
                }
                let (sub, amount) = w.extract(
                    endpoint,
                    n.resource,
                    n.substance,
                    n.resource.pull_limit().min(free),
                );
                let valid = match n.resource {
                    Resource::Fluid => [3, 6].contains(&sub),
                    Resource::Gas => [4, 5].contains(&sub),
                    Resource::Energy => false,
                };
                if valid && (n.substance == 0 || sub == n.substance) {
                    let accepted = amount.min(free).min(n.resource.pull_limit());
                    n.stored += accepted;
                    pulled += accepted;
                    if accepted > 0 {
                        n.substance = sub;
                    }
                }
            }
        }
        let fill = (u128::from(n.stored.min(n.capacity())) * 1000 / u128::from(n.capacity().max(1)))
            as u16;
        let before = n.glow;
        n.glow = n.glow.max(fill);
        if n.sent {
            n.glow = (n.glow + 20).min(1000);
        } else {
            n.glow = n.glow.saturating_sub(20).max(fill);
        }
        for (i, &endpoint) in n.endpoints.iter().enumerate() {
            n.requests[i] = if !endpoint.pull && w.active(endpoint) && n.stored > 0 {
                work.endpoint_queries += 1;
                w.room(endpoint, n.resource, n.substance)
            } else {
                0
            };
        }
        fair_share(n.stored, &n.requests, &mut n.grants)
            .map_err(|_| GridError::InvalidEndpoints)?;
        let mut sent = 0;
        for (i, &endpoint) in n.endpoints.iter().enumerate() {
            if n.grants[i] > 0 {
                work.transfers += 1;
                sent += w
                    .give(endpoint, n.resource, n.substance, n.grants[i])
                    .min(n.grants[i]);
            }
        }
        n.stored -= sent;
        n.flow = sent;
        n.sent = sent > 0;
        if n.resource != Resource::Energy && n.stored == 0 && !n.sent {
            n.substance = 0;
        }
        Ok(sent > 0 || pulled > 0 || n.glow != before)
    }
}
