//! Deterministic bounded BFS. Reserving receiver capacity is a callback, so the
//! same search observes earlier detached transfers of the authoritative tick.
use super::*;
pub trait RouteWorld {
    fn cell(&mut self, position: BlockPos) -> Result<Cell, MachineError>;
    fn color(&self, position: BlockPos) -> u8;
    fn capacity(
        &mut self,
        position: BlockPos,
        face: Face,
        stack: ItemStack,
        skip: Option<u64>,
    ) -> Result<Option<(ComponentId, u16)>, MachineError>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub path: Vec<BlockPos>,
    pub destination: ComponentId,
    pub amount: u16,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RouteWork {
    pub nodes: u64,
    pub candidates: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct RouteBudget {
    pub nodes: usize,
    pub candidates: usize,
}
/// `only` selects the home fallback; otherwise `home` is excluded from targets.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit bounded route authority and independent work accounting"
)]
pub fn find_route(
    w: &mut impl RouteWorld,
    start: BlockPos,
    color: u8,
    stack: ItemStack,
    home: Option<BlockPos>,
    only: Option<BlockPos>,
    skip: Option<u64>,
    budget: RouteBudget,
    work: &mut RouteWork,
) -> Result<Option<Route>, MachineError> {
    if color > 18 || stack.is_empty() || budget.nodes == 0 {
        return Err(MachineError::Bounds);
    }
    if w.cell(start)?.id() != LOGISTICAL_TRANSPORTER {
        return Ok(None);
    }
    let mut seen = BTreeMap::from([(start, (0usize, start))]);
    let mut order = vec![start];
    let mut cursor = 0;
    let mut best: Option<(usize, BlockPos, Face, BlockPos, ComponentId, u16)> = None;
    work.nodes += 1;
    while let Some(&p) = order.get(cursor) {
        cursor += 1;
        let distance = seen[&p].0;
        if best.is_some_and(|b| distance > b.0) {
            break;
        }
        let cell = w.cell(p)?;
        let a = arms::arms_of(cell.state());
        for face in Face::ALL {
            let code = arms::arm(a, face as u8);
            if code == 0 || code == 3 && only.is_none() {
                continue;
            }
            let q = offset(p, face)?;
            let neighbor = w.cell(q)?;
            if neighbor.id() == LOGISTICAL_TRANSPORTER {
                if code != 1
                    || seen.contains_key(&q)
                    || arms::arm(arms::arms_of(neighbor.state()), face.opposite() as u8) != 1
                    || !(w.color(q) == 0 || w.color(q) == color)
                    || best.is_some_and(|b| distance >= b.0)
                {
                    continue;
                }
                if seen.len() >= budget.nodes {
                    return Err(MachineError::Bounds);
                }
                seen.insert(q, (distance + 1, p));
                work.nodes += 1;
                order.push(q);
                continue;
            }
            if distance + 2 > MAX_PACKET_PATH
                || only.is_some_and(|target| target != q)
                || only.is_none() && Some(q) == home
            {
                continue;
            }
            if work.candidates >= budget.candidates as u64 {
                return Err(MachineError::Bounds);
            }
            work.candidates += 1;
            if let Some((destination, amount)) = w.capacity(q, face.opposite(), stack, skip)?
                && amount > 0
            {
                let candidate = (
                    distance,
                    q,
                    face.opposite(),
                    p,
                    destination,
                    amount.min(stack.count()),
                );
                if best.is_none_or(|b| {
                    (candidate.0, candidate.1, candidate.2 as u8) < (b.0, b.1, b.2 as u8)
                }) {
                    best = Some(candidate);
                }
            }
        }
    }
    let Some((_, target, _, end, destination, amount)) = best else {
        return Ok(None);
    };
    let mut path = vec![end];
    let mut p = end;
    while p != start {
        p = seen[&p].1;
        path.push(p);
    }
    path.reverse();
    path.push(target);
    Ok(Some(Route {
        path,
        destination,
        amount,
    }))
}
