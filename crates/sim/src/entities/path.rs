//! Incremental bounded land navigation. No world is owned or generated. Each
//! advance has logical budgets and can pause for a missing known snapshot.
use crate::QueryWorld;
use core_types::Cell;
use rules::{
    blocks::{bdef, collision_of},
    entities::{EntityError, MobKind},
    ids::*,
};
use std::{cmp::Ordering, collections::BinaryHeap};

#[derive(Clone, Copy, Debug)]
pub struct NavSpec {
    pub height: u8,
    pub range: i32,
    pub maximum_nodes: usize,
    pub safe_fall: u8,
    pub water_penalty: u32,
    pub swims: bool,
}
impl NavSpec {
    pub fn for_mob(kind: MobKind, baby: bool) -> Self {
        let definition = kind.definition();
        Self {
            height: (definition.height * if baby { 0.5 } else { 1.0 }).ceil() as u8,
            range: definition.follow as i32,
            maximum_nodes: (definition.follow * 16.0) as usize,
            safe_fall: 3,
            water_penalty: (definition.water_penalty * 10.0) as u32,
            swims: kind != MobKind::Zombie,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct PathBudget {
    pub expansions: u32,
    pub cells: u32,
}
impl Default for PathBudget {
    fn default() -> Self {
        Self {
            expansions: 32,
            cells: 16_384,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PathWork {
    pub expanded: u32,
    pub reads: u32,
    pub popped: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchStatus {
    Pending,
    Found,
    Partial,
    Empty,
    WaitingForKnown([i32; 3]),
    Cancelled,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavPath {
    points: [[i32; 3]; 128],
    len: u16,
    pub reached: bool,
}
impl NavPath {
    pub fn points(&self) -> &[[i32; 3]] {
        &self.points[..usize::from(self.len)]
    }
}
#[derive(Clone, Copy, Debug)]
struct Node {
    position: [i32; 3],
    cost: u32,
    parent: Option<u16>,
    closed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Open {
    score: u32,
    heuristic: u32,
    index: u16,
    cost: u32,
}
impl Ord for Open {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .cmp(&self.score)
            .then_with(|| other.heuristic.cmp(&self.heuristic))
            .then_with(|| other.index.cmp(&self.index))
            .then_with(|| other.cost.cmp(&self.cost))
    }
}
impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
pub struct NavSearch {
    start: [i32; 3],
    goal: [i32; 3],
    spec: NavSpec,
    epoch: u64,
    nodes: Vec<Node>,
    indices: Vec<Option<([i32; 3], u16)>>,
    heap: BinaryHeap<Open>,
    scratch: Vec<[i32; 3]>,
    closest: u16,
    result: Option<NavPath>,
    status: SearchStatus,
}
impl NavSearch {
    pub fn new(
        start: [i32; 3],
        goal: [i32; 3],
        spec: NavSpec,
        source_epoch: u64,
    ) -> Result<Self, EntityError> {
        if spec.maximum_nodes == 0
            || spec.maximum_nodes > 4096
            || spec.height == 0
            || spec.height > 4
            || spec.range <= 0
            || spec.range > 128
            || spec.safe_fall > 16
            || spec.water_penalty > 1_000_000
        {
            return Err(EntityError::Capacity);
        }
        if start
            .iter()
            .chain(&goal)
            .any(|&value| !(i32::MIN + 1024..=i32::MAX - 1024).contains(&value))
        {
            return Err(EntityError::InvalidPosition);
        }
        let mut search = Self {
            start,
            goal,
            spec,
            epoch: source_epoch,
            nodes: Vec::with_capacity(spec.maximum_nodes),
            indices: vec![None; (spec.maximum_nodes * 2).next_power_of_two()],
            heap: BinaryHeap::with_capacity(spec.maximum_nodes * 8),
            scratch: Vec::with_capacity(spec.maximum_nodes),
            closest: 0,
            result: None,
            status: SearchStatus::Pending,
        };
        search.add_node(start, 0, None)?;
        Ok(search)
    }
    pub const fn status(&self) -> SearchStatus {
        self.status
    }
    pub fn result(&self) -> Option<&NavPath> {
        self.result.as_ref()
    }
    pub const fn source_epoch(&self) -> u64 {
        self.epoch
    }
    pub fn cancel_if_epoch(&mut self, epoch: u64) -> bool {
        if epoch != self.epoch {
            self.status = SearchStatus::Cancelled;
            self.result = None;
            true
        } else {
            false
        }
    }
    fn heuristic(&self, p: [i32; 3]) -> u32 {
        (0..3)
            .map(|axis| self.goal[axis].abs_diff(p[axis]))
            .fold(0u32, u32::saturating_add)
            .saturating_mul(10)
    }
    fn hash(position: [i32; 3]) -> usize {
        let mut hash = 0x9e3779b97f4a7c15u64;
        for value in position {
            hash = (hash ^ (value as u32 as u64)).wrapping_mul(0xbf58476d1ce4e5b9);
            hash ^= hash >> 27;
        }
        hash as usize
    }
    fn slot(&self, position: [i32; 3]) -> usize {
        let start = Self::hash(position) & (self.indices.len() - 1);
        for offset in 0..self.indices.len() {
            let slot = (start + offset) & (self.indices.len() - 1);
            if self.indices[slot].is_none_or(|(key, _)| key == position) {
                return slot;
            }
        }
        unreachable!("node hash below half full")
    }
    fn add_node(
        &mut self,
        position: [i32; 3],
        cost: u32,
        parent: Option<u16>,
    ) -> Result<u16, EntityError> {
        if self.nodes.len() == self.spec.maximum_nodes
            || self.heap.len() == self.spec.maximum_nodes * 8
        {
            return Err(EntityError::Capacity);
        }
        let index = self.nodes.len() as u16;
        let heuristic = self.heuristic(position);
        self.nodes.push(Node {
            position,
            cost,
            parent,
            closed: false,
        });
        let slot = self.slot(position);
        self.indices[slot] = Some((position, index));
        self.heap.push(Open {
            score: cost.saturating_add(heuristic.saturating_mul(3) / 2),
            heuristic,
            index,
            cost,
        });
        Ok(index)
    }
    fn finish(&mut self, index: u16, reached: bool) {
        self.scratch.clear();
        let mut current = Some(index);
        while let Some(index) = current {
            let node = self.nodes[index as usize];
            self.scratch.push(node.position);
            current = node.parent;
            debug_assert!(self.scratch.len() <= self.spec.maximum_nodes);
        }
        let count = self.scratch.len().min(128);
        let mut path = NavPath {
            points: [[0; 3]; 128],
            len: count as u16,
            reached: reached && self.scratch.len() <= 128,
        };
        for (destination, source) in path
            .points
            .iter_mut()
            .zip(self.scratch.iter().rev())
            .take(count)
        {
            *destination = *source;
        }
        self.status = if path.reached {
            SearchStatus::Found
        } else {
            SearchStatus::Partial
        };
        self.result = Some(path);
    }
    pub fn advance(&mut self, world: &impl QueryWorld, budget: PathBudget) -> PathWork {
        let mut work = PathWork::default();
        if matches!(
            self.status,
            SearchStatus::Found
                | SearchStatus::Partial
                | SearchStatus::Empty
                | SearchStatus::Cancelled
        ) {
            return work;
        }
        self.status = SearchStatus::Pending;
        while work.expanded < budget.expansions
            && budget.cells.saturating_sub(work.reads) >= 512
            && work.popped < budget.expansions.saturating_mul(8).saturating_add(1)
        {
            let Some(open) = self.heap.pop() else {
                if self.closest == 0 {
                    self.status = SearchStatus::Empty;
                } else {
                    self.finish(self.closest, false);
                }
                break;
            };
            work.popped += 1;
            let node = self.nodes[open.index as usize];
            if node.closed || node.cost != open.cost {
                continue;
            }
            if node.position == self.goal {
                self.finish(open.index, true);
                break;
            }
            let mut query = Probe { world, reads: 0 };
            let neighbors = neighbors(&mut query, node.position, self.spec);
            work.reads += query.reads;
            let neighbors = match neighbors {
                Ok(value) => value,
                Err(missing) => {
                    self.heap.push(open);
                    self.status = SearchStatus::WaitingForKnown(missing);
                    break;
                }
            };
            self.nodes[open.index as usize].closed = true;
            work.expanded += 1;
            for (position, penalty, diagonal) in neighbors.into_iter().flatten() {
                if (position[0] - self.start[0]).abs() > self.spec.range
                    || (position[2] - self.start[2]).abs() > self.spec.range
                    || (position[1] - self.start[1]).abs() > 16
                {
                    continue;
                }
                let movement = if diagonal { 14 } else { 10 };
                let cost = node.cost.saturating_add(
                    movement + penalty + position[1].abs_diff(node.position[1]) * 10,
                );
                let slot = self.slot(position);
                let index = if let Some((_, index)) = self.indices[slot] {
                    let target = &mut self.nodes[index as usize];
                    if target.closed || target.cost <= cost {
                        continue;
                    }
                    target.cost = cost;
                    target.parent = Some(open.index);
                    if self.heap.len() == self.spec.maximum_nodes * 8 {
                        self.finish(self.closest, false);
                        return work;
                    }
                    let heuristic = self.heuristic(position);
                    self.heap.push(Open {
                        score: cost.saturating_add(heuristic.saturating_mul(3) / 2),
                        heuristic,
                        index,
                        cost,
                    });
                    index
                } else {
                    match self.add_node(position, cost, Some(open.index)) {
                        Ok(index) => index,
                        Err(_) => {
                            self.finish(self.closest, false);
                            return work;
                        }
                    }
                };
                let best = self.nodes[self.closest as usize];
                if self.heuristic(position) < self.heuristic(best.position)
                    || (self.heuristic(position) == self.heuristic(best.position)
                        && cost < best.cost)
                {
                    self.closest = index;
                }
            }
        }
        work
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Open,
    Water,
    Lava,
    Blocked,
    Leaves,
    Cactus,
}
struct Probe<'a, W: QueryWorld> {
    world: &'a W,
    reads: u32,
}
impl<W: QueryWorld> Probe<'_, W> {
    fn read(&mut self, p: [i32; 3]) -> Result<Cell, [i32; 3]> {
        self.reads += 1;
        match self.world.cell(p) {
            world::CellRead::Known(cell) => Ok(cell),
            world::CellRead::Unknown => Err(p),
        }
    }
    fn kind(&mut self, p: [i32; 3]) -> Result<Kind, [i32; 3]> {
        let cell = self.read(p)?;
        let definition = bdef(cell.id());
        Ok(if cell.id() == CACTUS {
            Kind::Cactus
        } else if definition.leaves {
            Kind::Leaves
        } else if definition.fluid == rules::blocks::FLUID_WATER {
            Kind::Water
        } else if definition.fluid == rules::blocks::FLUID_LAVA {
            Kind::Lava
        } else if collision_of(cell.raw())
            .iter()
            .any(|shape| shape.hi[1] > 0.5)
        {
            Kind::Blocked
        } else {
            Kind::Open
        })
    }
    fn penalty(&mut self, p: [i32; 3], spec: NavSpec) -> Result<Option<u32>, [i32; 3]> {
        let mut wet = false;
        for dy in 0..spec.height {
            match self.kind([p[0], p[1] + i32::from(dy), p[2]])? {
                Kind::Open => {}
                Kind::Water => wet = true,
                _ => return Ok(None),
            }
        }
        match self.kind([p[0], p[1] - 1, p[2]])? {
            Kind::Blocked | Kind::Leaves => {}
            Kind::Water if spec.swims => wet = true,
            Kind::Open if wet => {}
            _ => return Ok(None),
        }
        let mut penalty = if wet { spec.water_penalty } else { 0 };
        for dz in -1..=1 {
            for dx in -1..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                for dy in -1..=0 {
                    match self.kind([p[0] + dx, p[1] + dy, p[2] + dz])? {
                        Kind::Cactus | Kind::Lava => penalty = penalty.max(80),
                        Kind::Water if dy == -1 => penalty = penalty.max(spec.water_penalty),
                        _ => {}
                    }
                }
            }
        }
        Ok(Some(penalty))
    }
    fn walk(&mut self, p: [i32; 3], spec: NavSpec) -> Result<Option<([i32; 3], u32)>, [i32; 3]> {
        if let Some(penalty) = self.penalty(p, spec)? {
            return Ok(Some((p, penalty)));
        }
        if matches!(self.kind(p)?, Kind::Blocked) {
            let up = [p[0], p[1] + 1, p[2]];
            return Ok(self.penalty(up, spec)?.map(|penalty| (up, penalty)));
        }
        if !matches!(self.kind(p)?, Kind::Open | Kind::Water) {
            return Ok(None);
        }
        for distance in 1..=spec.safe_fall {
            let down = [p[0], p[1] - i32::from(distance), p[2]];
            if let Some(penalty) = self.penalty(down, spec)? {
                return Ok(Some((down, penalty)));
            }
            if !matches!(self.kind(down)?, Kind::Open | Kind::Water) {
                break;
            }
        }
        Ok(None)
    }
}
type Neighbor = ([i32; 3], u32, bool);
fn neighbors<W: QueryWorld>(
    query: &mut Probe<'_, W>,
    p: [i32; 3],
    spec: NavSpec,
) -> Result<[Option<Neighbor>; 8], [i32; 3]> {
    let mut out = [None; 8];
    let directions = [[0, -1], [1, 0], [0, 1], [-1, 0]];
    for (index, [dx, dz]) in directions.into_iter().enumerate() {
        out[index] = query
            .walk([p[0] + dx, p[1], p[2] + dz], spec)?
            .map(|(position, penalty)| (position, penalty, false));
        if out[index].is_some_and(|(target, _, _)| target[1] > p[1])
            && !matches!(
                query.kind([p[0], p[1] + i32::from(spec.height), p[2]])?,
                Kind::Open | Kind::Water
            )
        {
            out[index] = None;
        }
    }
    for (index, (a, b)) in [(0, 1), (1, 2), (2, 3), (3, 0)].into_iter().enumerate() {
        if out[a].is_some_and(|(position, _, _)| position[1] <= p[1])
            && out[b].is_some_and(|(position, _, _)| position[1] <= p[1])
        {
            let target = [
                p[0] + directions[a][0] + directions[b][0],
                p[1],
                p[2] + directions[a][1] + directions[b][1],
            ];
            out[index + 4] = query
                .walk(target, spec)?
                .filter(|(position, _)| position[1] <= p[1])
                .map(|(position, penalty)| (position, penalty, true));
        }
    }
    Ok(out)
}
pub fn waypoint(world: &impl QueryWorld, p: [i32; 3]) -> Result<[f64; 3], EntityError> {
    let world::CellRead::Known(cell) = world.cell(p) else {
        return Err(EntityError::UnknownInput);
    };
    let top = collision_of(cell.raw())
        .iter()
        .map(|shape| shape.hi[1])
        .fold(0.0f64, f64::max);
    Ok([
        f64::from(p[0]) + 0.5,
        f64::from(p[1]) + top,
        f64::from(p[2]) + 0.5,
    ])
}
