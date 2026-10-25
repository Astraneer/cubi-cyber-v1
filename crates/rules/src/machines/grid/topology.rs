//! Detached edits copy only adjacent connected components and at most seven free
//! slot headers per requested edit. Publication checks exact before-states before its first write.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopologyEdit {
    Add {
        position: Position,
        resource: Resource,
        links: u8,
    },
    Remove {
        position: Position,
    },
    Relink {
        position: Position,
        links: u8,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopologyBudget {
    pub nodes: u64,
    pub endpoints: u64,
    pub bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedSlot {
    pub index: u32,
    pub state: Option<SlotSnapshot>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopologyRecords {
    pub edit: TopologyEdit,
    pub extra_edits: Vec<TopologyEdit>,
    pub slots_len_before: u32,
    pub slots_len_after: u32,
    pub before_nodes: Vec<NodeSnapshot>,
    pub after_nodes: Vec<NodeSnapshot>,
    pub before_slots: Vec<IndexedSlot>,
    pub after_slots: Vec<IndexedSlot>,
    pub overflow: u64,
}
pub struct TopologyDelta {
    records: TopologyRecords,
}
impl TopologyDelta {
    pub fn records(&self) -> &TopologyRecords {
        &self.records
    }
    pub fn overflow(&self) -> u64 {
        self.records.overflow
    }
    pub fn affected_before(&self) -> impl Iterator<Item = NetworkId> + '_ {
        live_ids(&self.records.before_slots)
    }
    pub fn affected_after(&self) -> impl Iterator<Item = NetworkId> + '_ {
        live_ids(&self.records.after_slots)
    }
}
fn live_ids(slots: &[IndexedSlot]) -> impl Iterator<Item = NetworkId> + '_ {
    slots.iter().filter_map(|slot| {
        slot.state
            .as_ref()
            .filter(|s| s.network.is_some())
            .map(|s| NetworkId {
                slot: slot.index,
                generation: s.generation,
            })
    })
}
fn node(position: Position, member: Member) -> NodeSnapshot {
    NodeSnapshot {
        position,
        network: member.network,
        resource: member.resource,
        links: member.links,
    }
}
fn state(slot: &Slot) -> SlotSnapshot {
    SlotSnapshot {
        generation: slot.generation,
        network: slot.network.as_ref().map(|n| NetworkSnapshot {
            resource: n.resource,
            reservoir: n.reservoir(),
            members: n.members.clone(),
            endpoints: n.endpoints.clone(),
        }),
    }
}
fn matches_slot(slot: &Slot, snapshot: &SlotSnapshot) -> bool {
    slot.generation == snapshot.generation
        && match (&slot.network, &snapshot.network) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                a.resource == b.resource
                    && a.reservoir() == b.reservoir
                    && a.members == b.members
                    && a.endpoints == b.endpoints
            }
            _ => false,
        }
}
fn empty(resource: Resource) -> Network {
    Network {
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
    }
}
struct Projection {
    members: BTreeMap<Position, Member>,
    slots: BTreeMap<u32, Slot>,
    free: BTreeSet<u32>,
    slot_len: usize,
    max_nodes: usize,
}
impl Projection {
    fn network(&self, id: NetworkId) -> &Network {
        self.slots[&id.slot]
            .network
            .as_ref()
            .expect("projected live component")
    }
    fn network_mut(&mut self, id: NetworkId) -> &mut Network {
        self.slots
            .get_mut(&id.slot)
            .expect("projected slot")
            .network
            .as_mut()
            .expect("projected live component")
    }
    fn allocate(&mut self, resource: Resource) -> Result<NetworkId, GridError> {
        let index = if let Some(index) = self.free.pop_first() {
            index
        } else {
            if self.slot_len >= self.max_nodes {
                return Err(GridError::Capacity);
            }
            let index = u32::try_from(self.slot_len).map_err(|_| GridError::Capacity)?;
            self.slot_len += 1;
            self.slots.insert(
                index,
                Slot {
                    generation: 0,
                    network: None,
                },
            );
            index
        };
        let slot = self.slots.get_mut(&index).ok_or(GridError::Stale)?;
        slot.generation = slot.generation.checked_add(1).ok_or(GridError::Capacity)?;
        slot.network = Some(empty(resource));
        Ok(NetworkId {
            slot: index,
            generation: slot.generation,
        })
    }
    fn connected(&self, a: Position, b: Position, face: Face) -> bool {
        self.members
            .get(&a)
            .zip(self.members.get(&b))
            .is_some_and(|(a, b)| {
                a.resource == b.resource
                    && a.links & (1 << face as u8) != 0
                    && b.links & (1 << face.opposite() as u8) != 0
            })
    }
    fn merge(&mut self, target: NetworkId, source: NetworkId, work: &mut GridWork) {
        let other = self
            .slots
            .get_mut(&source.slot)
            .unwrap()
            .network
            .take()
            .unwrap();
        if source.generation < u32::MAX {
            self.free.insert(source.slot);
        }
        for p in &other.members {
            self.members.get_mut(p).unwrap().network = target;
        }
        work.topology_nodes += other.members.len() as u64;
        let n = self.network_mut(target);
        let a = u128::from(n.capacity());
        let b = u128::from(other.capacity());
        n.glow = ((u128::from(n.glow) * a + u128::from(other.glow) * b) / (a + b)) as u16;
        n.stored += other.stored;
        if n.substance == 0 {
            n.substance = other.substance;
        }
        n.members.extend(other.members);
        n.members.sort_unstable();
        n.endpoints.clear();
        n.requests.clear();
        n.grants.clear();
    }
    fn reconcile(&mut self, position: Position, work: &mut GridWork) -> Result<(), GridError> {
        let own = self
            .members
            .get(&position)
            .ok_or(GridError::Missing)?
            .network;
        let mut ids = vec![own];
        for face in Face::ALL {
            if let Some(q) = sides::offset(position, face)
                && self.connected(position, q, face)
            {
                let id = self.members[&q].network;
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        ids.sort_by(|a, b| {
            self.network(*b)
                .members
                .len()
                .cmp(&self.network(*a).members.len())
                .then(a.cmp(b))
        });
        let mut keep = own;
        for id in ids {
            if id == keep || self.slots[&id.slot].network.is_none() {
                continue;
            }
            let a = self.network(keep);
            let b = self.network(id);
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
            self.merge(target, source, work);
            keep = target;
        }
        Ok(())
    }
    fn repartition(
        &mut self,
        id: NetworkId,
        removed: Option<Position>,
        work: &mut GridWork,
    ) -> Result<u64, GridError> {
        let old = self
            .slots
            .get_mut(&id.slot)
            .unwrap()
            .network
            .take()
            .unwrap();
        let remaining: Vec<_> = old
            .members
            .iter()
            .copied()
            .filter(|p| Some(*p) != removed)
            .collect();
        if remaining.is_empty() {
            if id.generation < u32::MAX {
                self.free.insert(id.slot);
            }
            return Ok(old.stored);
        }
        let mut seen = BTreeSet::new();
        let mut parts = Vec::new();
        for &seed in &remaining {
            if !seen.insert(seed) {
                continue;
            }
            let mut part = vec![seed];
            let mut head = 0;
            while head < part.len() {
                let p = part[head];
                head += 1;
                work.topology_nodes += 1;
                for face in Face::ALL {
                    if let Some(q) = sides::offset(p, face)
                        && self.connected(p, q, face)
                        && self.members[&q].network == id
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
        let mut kept = old.clone();
        kept.members = parts[0].clone();
        kept.stored = 0;
        kept.endpoints.clear();
        kept.requests.clear();
        kept.grants.clear();
        self.slots.get_mut(&id.slot).unwrap().network = Some(kept);
        for part in parts.into_iter().skip(1) {
            let new = self.allocate(old.resource)?;
            let share =
                (u128::from(stored) * part.len() as u128 * old.resource.unit_capacity() as u128
                    / u128::from(capacity)) as u64;
            assigned += share;
            for p in &part {
                self.members.get_mut(p).unwrap().network = new;
            }
            let n = self.network_mut(new);
            n.members = part;
            n.stored = share;
            n.substance = if share > 0 { old.substance } else { 0 };
            n.glow = old.glow;
            n.sent = old.sent;
        }
        let n = self.network_mut(id);
        n.stored = stored - assigned;
        n.substance = if n.stored > 0 { old.substance } else { 0 };
        Ok(lost)
    }
}
impl Grid {
    pub fn plan_edit(
        &self,
        edit: TopologyEdit,
        budget: TopologyBudget,
        work: &mut GridWork,
    ) -> Result<TopologyDelta, GridError> {
        self.plan_edits(std::slice::from_ref(&edit), budget, work)
    }
    /// Up to 64 edits publish one final connected topology. Installing a cable
    /// and changing its six neighbors never exposes an intermediate graph.
    pub fn plan_edits(
        &self,
        edits: &[TopologyEdit],
        budget: TopologyBudget,
        work: &mut GridWork,
    ) -> Result<TopologyDelta, GridError> {
        if edits.is_empty() || edits.len() > 64 {
            return Err(GridError::Capacity);
        }
        let mut virtual_nodes = BTreeMap::<Position, Option<Resource>>::new();
        let mut forms = Vec::with_capacity(edits.len());
        let mut ids = BTreeSet::new();
        let mut node_count = self.members.len();
        for &edit in edits {
            let position = match edit {
                TopologyEdit::Add { position, .. }
                | TopologyEdit::Remove { position }
                | TopologyEdit::Relink { position, .. } => position,
            };
            let old = virtual_nodes
                .get(&position)
                .copied()
                .unwrap_or_else(|| self.members.get(&position).map(|m| m.resource));
            let (resource, links) = match edit {
                TopologyEdit::Add {
                    resource, links, ..
                } => {
                    if old.is_some() {
                        return Err(GridError::Exists);
                    }
                    if node_count >= self.max_nodes {
                        return Err(GridError::Capacity);
                    }
                    node_count += 1;
                    virtual_nodes.insert(position, Some(resource));
                    (resource, links)
                }
                TopologyEdit::Remove { .. } => {
                    let resource = old.ok_or(GridError::Missing)?;
                    node_count -= 1;
                    virtual_nodes.insert(position, None);
                    (resource, 0)
                }
                TopologyEdit::Relink { links, .. } => (old.ok_or(GridError::Missing)?, links),
            };
            if links & !63 != 0 {
                return Err(GridError::Capacity);
            }
            if let Some(member) = self.members.get(&position) {
                ids.insert(member.network);
            }
            if !matches!(edit, TopologyEdit::Remove { .. }) {
                for face in Face::ALL {
                    if links & (1 << face as u8) == 0 {
                        continue;
                    }
                    if let Some(q) = sides::offset(position, face)
                        && let Some(member) = self.members.get(&q)
                        && member.resource == resource
                        && member.links & (1 << face.opposite() as u8) != 0
                    {
                        ids.insert(member.network);
                    }
                }
            }
            forms.push((edit, position, resource, links));
        }
        let mut nodes = 0_u64;
        let mut endpoints = 0_u64;
        for id in &ids {
            let n = self.network(*id)?;
            nodes = nodes
                .checked_add(n.members.len() as u64)
                .ok_or(GridError::Capacity)?;
            endpoints = endpoints
                .checked_add(n.endpoints.len() as u64)
                .ok_or(GridError::Capacity)?;
        }
        let visits = nodes
            .checked_mul(4)
            .and_then(|n| n.checked_add(1))
            .and_then(|n| n.checked_mul(edits.len() as u64))
            .ok_or(GridError::Capacity)?;
        let bytes = nodes
            .checked_add(edits.len() as u64)
            .and_then(|n| n.checked_mul(512))
            .and_then(|n| endpoints.checked_mul(256).and_then(|e| n.checked_add(e)))
            .and_then(|n| n.checked_add(8192 * edits.len() as u64))
            .ok_or(GridError::Capacity)?;
        if visits > budget.nodes || endpoints > budget.endpoints || bytes > budget.bytes {
            return Err(GridError::Capacity);
        }
        let mut projected = Projection {
            members: BTreeMap::new(),
            slots: BTreeMap::new(),
            free: self
                .free_slots
                .iter()
                .copied()
                .take(7 * edits.len())
                .collect(),
            slot_len: self.slots.len(),
            max_nodes: self.max_nodes,
        };
        for id in &ids {
            let n = self.network(*id)?;
            for &p in &n.members {
                projected.members.insert(p, self.members[&p]);
            }
            projected
                .slots
                .insert(id.slot, self.slots[id.slot as usize].clone());
        }
        for &index in &projected.free {
            projected
                .slots
                .insert(index, self.slots[index as usize].clone());
        }
        let before_nodes = projected
            .members
            .iter()
            .map(|(&p, &m)| node(p, m))
            .collect();
        let mut actual = GridWork {
            topology_nodes: nodes,
            ..GridWork::default()
        };
        let mut overflow = 0_u64;
        for (edit, position, resource, links) in forms {
            let lost = match edit {
                TopologyEdit::Add { .. } => {
                    let id = projected.allocate(resource)?;
                    projected.members.insert(
                        position,
                        Member {
                            network: id,
                            resource,
                            links,
                        },
                    );
                    projected.network_mut(id).members.push(position);
                    actual.topology_nodes += 1;
                    projected.reconcile(position, &mut actual)?;
                    0
                }
                TopologyEdit::Remove { .. } => {
                    let m = projected.members.remove(&position).unwrap();
                    projected.repartition(m.network, Some(position), &mut actual)?
                }
                TopologyEdit::Relink { .. } => {
                    let m = projected.members.get_mut(&position).unwrap();
                    m.links = links;
                    let id = m.network;
                    let overflow = projected.repartition(id, None, &mut actual)?;
                    projected.reconcile(position, &mut actual)?;
                    overflow
                }
            };
            overflow = overflow.checked_add(lost).ok_or(GridError::Capacity)?;
        }
        let mut before_slots = Vec::new();
        let mut after_slots = Vec::new();
        for (&index, slot) in &projected.slots {
            before_slots.push(IndexedSlot {
                index,
                state: self.slots.get(index as usize).map(state),
            });
            after_slots.push(IndexedSlot {
                index,
                state: Some(state(slot)),
            });
        }
        work.topology_nodes += actual.topology_nodes;
        work.visited_networks += ids.len() as u64;
        Ok(TopologyDelta {
            records: TopologyRecords {
                edit: edits[0],
                extra_edits: edits[1..].to_vec(),
                slots_len_before: u32::try_from(self.slots.len())
                    .map_err(|_| GridError::Capacity)?,
                slots_len_after: u32::try_from(projected.slot_len)
                    .map_err(|_| GridError::Capacity)?,
                before_nodes,
                after_nodes: projected
                    .members
                    .into_iter()
                    .map(|(p, m)| node(p, m))
                    .collect(),
                before_slots,
                after_slots,
                overflow,
            },
        })
    }
    pub fn restore_topology_delta(
        &self,
        records: TopologyRecords,
        budget: TopologyBudget,
        work: &mut GridWork,
    ) -> Result<TopologyDelta, GridError> {
        if records.extra_edits.len() >= 64 {
            return Err(GridError::Capacity);
        }
        let edits: Vec<_> = std::iter::once(records.edit)
            .chain(records.extra_edits.iter().copied())
            .collect();
        let planned = self.plan_edits(&edits, budget, work)?;
        if planned.records != records {
            return Err(GridError::Stale);
        }
        Ok(planned)
    }
    pub fn validate_edit(&self, delta: &TopologyDelta) -> Result<(), GridError> {
        let r = &delta.records;
        if self.slots.len() != r.slots_len_before as usize {
            return Err(GridError::Stale);
        }
        for n in &r.before_nodes {
            if self
                .members
                .get(&n.position)
                .is_none_or(|m| node(n.position, *m) != *n)
            {
                return Err(GridError::Stale);
            }
        }
        for n in &r.after_nodes {
            if r.before_nodes
                .binary_search_by_key(&n.position, |n| n.position)
                .is_err()
                && self.members.contains_key(&n.position)
            {
                return Err(GridError::Stale);
            }
        }
        for slot in &r.before_slots {
            if !match (self.slots.get(slot.index as usize), &slot.state) {
                (None, None) => true,
                (Some(a), Some(b)) => matches_slot(a, b),
                _ => false,
            } {
                return Err(GridError::Stale);
            }
        }
        let count = self.members.len() - r.before_nodes.len() + r.after_nodes.len();
        if count > self.max_nodes {
            return Err(GridError::Capacity);
        }
        Ok(())
    }
    pub fn apply_edit(&mut self, delta: TopologyDelta) -> Result<(), GridError> {
        self.validate_edit(&delta)?;
        let records = delta.records;
        for n in records.before_nodes {
            self.members.remove(&n.position);
        }
        for n in records.after_nodes {
            self.members.insert(
                n.position,
                Member {
                    network: n.network,
                    resource: n.resource,
                    links: n.links,
                },
            );
        }
        self.slots
            .resize_with(records.slots_len_after as usize, || Slot {
                generation: 0,
                network: None,
            });
        for indexed in records.after_slots {
            let s = indexed.state.expect("planned slot exists");
            let network = s.network.map(|n| Network {
                resource: n.resource,
                substance: n.reservoir.substance,
                stored: n.reservoir.stored,
                glow: n.reservoir.glow,
                flow: n.reservoir.flow,
                sent: n.reservoir.sent,
                members: n.members,
                requests: vec![0; n.endpoints.len()],
                grants: vec![0; n.endpoints.len()],
                endpoints: n.endpoints,
            });
            if network.is_none() && s.generation < u32::MAX {
                self.free_slots.insert(indexed.index);
            } else {
                self.free_slots.remove(&indexed.index);
            }
            self.slots[indexed.index as usize] = Slot {
                generation: s.generation,
                network,
            };
        }
        Ok(())
    }
}
