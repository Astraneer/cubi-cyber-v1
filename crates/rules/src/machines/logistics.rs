//! Conservative network transfers and fixed-slot item transit. Topology is
//! rebuilt only when connections change; transfers never conjure inventory.
use crate::stack::ItemStack;

/// Weighted max-min allocation without floats or allocation. Remainders go in
/// stable endpoint order. `granted` must have one element for each request.
pub fn fair_share(
    total: u64,
    requests: &[u64],
    granted: &mut [u64],
) -> Result<u64, LogisticsError> {
    if requests.len() != granted.len() {
        return Err(LogisticsError::Shape);
    }
    granted.fill(0);
    let mut left = total;
    loop {
        let count = requests
            .iter()
            .zip(granted.iter())
            .filter(|(r, g)| r > g)
            .count() as u64;
        if count == 0 || left == 0 {
            break;
        }
        let share = left / count;
        let mut spent = 0;
        if share == 0 {
            for (r, g) in requests.iter().zip(granted.iter_mut()) {
                if *r > *g && left > 0 {
                    *g += 1;
                    left -= 1;
                }
            }
            break;
        }
        for (r, g) in requests.iter().zip(granted.iter_mut()) {
            let n = (r - *g).min(share);
            *g += n;
            spent += n;
        }
        left -= spent;
    }
    Ok(total - left)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogisticsError {
    Shape,
    Capacity,
    StaleTopology,
    InvalidRoute,
    NoRoom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    pub id: u64,
    pub stack: ItemStack,
    pub route: u32,
    pub cell: u16,
    pub progress: u8,
    pub last_tick: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrival {
    pub packet_id: u64,
    pub stack: ItemStack,
    pub destination: u32,
}
pub const MAX_ROUTE: usize = 128;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub generation: u64,
    pub cells: [u32; MAX_ROUTE],
    pub count: u16,
    pub destination: u32,
}
impl Route {
    pub fn new(generation: u64, cells: &[u32], destination: u32) -> Result<Self, LogisticsError> {
        if cells.is_empty() || cells.len() > MAX_ROUTE {
            return Err(LogisticsError::InvalidRoute);
        }
        let mut route = Self {
            generation,
            cells: [0; MAX_ROUTE],
            count: cells.len() as u16,
            destination,
        };
        route.cells[..cells.len()].copy_from_slice(cells);
        Ok(route)
    }
}
pub struct Transit {
    packets: Vec<Packet>,
    routes: Vec<Route>,
    limit: usize,
    next_id: u64,
    pub topology_generation: u64,
}
impl Transit {
    pub fn new(limit: usize, max_routes: usize) -> Self {
        Self {
            packets: Vec::with_capacity(limit),
            routes: Vec::with_capacity(max_routes),
            limit,
            next_id: 1,
            topology_generation: 1,
        }
    }
    pub fn add_route(&mut self, route: Route) -> Result<u32, LogisticsError> {
        if route.generation != self.topology_generation {
            return Err(LogisticsError::StaleTopology);
        }
        if self.routes.len() == self.routes.capacity() {
            return Err(LogisticsError::Capacity);
        }
        let id = self.routes.len() as u32;
        self.routes.push(route);
        Ok(id)
    }
    /// Reservation is a prerequisite: source extraction must be committed only
    /// after this succeeds. No fallback drop on congestion or a stale route.
    pub fn insert(
        &mut self,
        stack: ItemStack,
        route: u32,
        tick: u64,
    ) -> Result<u64, LogisticsError> {
        if stack.is_empty() {
            return Err(LogisticsError::InvalidRoute);
        }
        let r = self
            .routes
            .get(route as usize)
            .ok_or(LogisticsError::InvalidRoute)?;
        if r.generation != self.topology_generation {
            return Err(LogisticsError::StaleTopology);
        }
        if self.packets.len() == self.limit || self.next_id == u64::MAX {
            return Err(LogisticsError::Capacity);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.packets.push(Packet {
            id,
            stack,
            route,
            cell: 0,
            progress: 0,
            last_tick: tick,
        });
        Ok(id)
    }
    /// Exactly one 5/100-cell movement per global tick, regardless of topology
    /// insertion order. Arrival stays reserved until the owner commits its ACK.
    pub fn tick(&mut self, tick: u64, arrivals: &mut Vec<Arrival>) -> Result<(), LogisticsError> {
        if arrivals.capacity() - arrivals.len() < self.packets.len() {
            return Err(LogisticsError::Capacity);
        }
        for packet in &mut self.packets {
            if packet.last_tick >= tick {
                continue;
            }
            let route = &self.routes[packet.route as usize];
            if route.generation != self.topology_generation {
                continue;
            }
            packet.last_tick = tick;
            if packet.progress < 100 {
                packet.progress = (packet.progress + 5).min(100);
            }
            if packet.progress == 100 {
                if packet.cell + 1 < route.count {
                    packet.cell += 1;
                    packet.progress = 0;
                } else {
                    arrivals.push(Arrival {
                        packet_id: packet.id,
                        stack: packet.stack,
                        destination: route.destination,
                    });
                }
            }
        }
        Ok(())
    }
    /// Only a transactionally accepted amount is removed. Partial ACK leaves
    /// the remainder at its destination, allowing a later dependency wake.
    pub fn acknowledge(&mut self, id: u64, accepted: u16) -> Result<(), LogisticsError> {
        let i = self
            .packets
            .iter()
            .position(|p| p.id == id)
            .ok_or(LogisticsError::InvalidRoute)?;
        let p = &mut self.packets[i];
        if accepted > p.stack.count() {
            return Err(LogisticsError::NoRoom);
        }
        p.stack = p
            .stack
            .with_count(p.stack.count() - accepted)
            .expect("lower count");
        if p.stack.is_empty() {
            self.packets.swap_remove(i);
        }
        Ok(())
    }
    pub fn packets(&self) -> &[Packet] {
        &self.packets
    }
    pub fn invalidate_topology(&mut self) -> Result<(), LogisticsError> {
        self.topology_generation = self
            .topology_generation
            .checked_add(1)
            .ok_or(LogisticsError::Capacity)?;
        Ok(())
    }
    pub fn reroute(&mut self, id: u64, route: u32) -> Result<(), LogisticsError> {
        let r = self
            .routes
            .get(route as usize)
            .ok_or(LogisticsError::InvalidRoute)?;
        if r.generation != self.topology_generation {
            return Err(LogisticsError::StaleTopology);
        }
        let p = self
            .packets
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(LogisticsError::InvalidRoute)?;
        p.route = route;
        p.cell = 0;
        p.progress = 0;
        Ok(())
    }
}
