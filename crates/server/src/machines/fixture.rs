//! Explicit startup fixture bridge. Live topology uses guarded topology plans.
use super::*;
impl MachineRuntime {
    pub fn register_transmitter_fixture(
        &mut self,
        world: &World,
        p: BlockPos,
        first: Tick,
    ) -> Result<(), MachineError> {
        let CellRead::Known(cell) = world.read(p) else {
            return Err(MachineError::Unknown);
        };
        let resource = match cell.id() {
            UNIVERSAL_CABLE => Resource::Energy,
            MECHANICAL_PIPE => Resource::Fluid,
            PRESSURIZED_TUBE => Resource::Gas,
            _ => return Err(MachineError::Component),
        };
        let grid = self
            .grids
            .entry(p.space)
            .or_insert_with(|| Grid::new(self.config.transmitters, self.config.endpoints));
        let mut previous = BTreeSet::new();
        for face in Face::ALL {
            if let Some(q) = sides::offset([p.x, p.y, p.z], face)
                && let Some(network) = grid.network_at(q)
            {
                previous.insert(NetworkKey {
                    space: p.space,
                    network,
                });
            }
        }
        let id = grid.add(
            [p.x, p.y, p.z],
            resource,
            arms::arm_mask(cell.state()),
            &mut GridWork::default(),
        )?;
        let key = NetworkKey {
            space: p.space,
            network: id,
        };
        if !self.network_orders.contains_key(&key) {
            self.network_orders.insert(key, self.next_order);
            self.next_order = self
                .next_order
                .checked_add(1)
                .ok_or(MachineError::Overflow)?;
        }
        // Joining networks retires their wake IDs; inspect the six neighboring
        // handles, never unrelated worlds or every component in the registry.
        let retired: Vec<_> = previous
            .into_iter()
            .filter(|key| {
                self.grids
                    .get(&p.space)
                    .is_some_and(|g| g.network(key.network).is_err())
            })
            .collect();
        for key in retired {
            self.remove_pending_indexed(Event::Network(key));
            self.network_orders.remove(&key);
            self.producers.remove(&key);
        }
        self.refresh_endpoints_fixture(world, key)?;
        let event = Pending {
            event: Event::Network(key),
            due: first,
            first_due: first,
            order: self.network_orders[&key],
        };
        self.insert_pending_indexed(event);
        Ok(())
    }
    fn refresh_endpoints_fixture(
        &mut self,
        world: &World,
        key: NetworkKey,
    ) -> Result<(), MachineError> {
        let grid = self.grids.get(&key.space).ok_or(MachineError::Missing)?;
        let network = grid.network(key.network)?;
        let resource = network.resource;
        let mut endpoints = Vec::new();
        let mut producers = BTreeSet::new();
        for &p in network.members() {
            let pos = BlockPos {
                space: key.space,
                x: p[0],
                y: p[1],
                z: p[2],
            };
            let CellRead::Known(cell) = world.read(pos) else {
                return Err(MachineError::Unknown);
            };
            let links = arms::arms_of(cell.state());
            for face in Face::ALL {
                let mode = arms::arm(links, face as u8);
                if mode == 0 {
                    continue;
                }
                let q = offset(pos, face)?;
                let CellRead::Known(neighbor) = world.read(q) else {
                    return Err(MachineError::Unknown);
                };
                let Some(BeKind::Mek(kind)) = blocks::bdef(neighbor.id()).be else {
                    continue;
                };
                let Some(id) = world
                    .page(q.page())
                    .and_then(|page| page.component(q.local_index()))
                else {
                    return Err(MachineError::Component);
                };
                if !self.machines.contains_key(&id) {
                    return Err(MachineError::Missing);
                }
                let target_face = face.opposite();
                let facing = Face::ALL[blocks::facing_of(neighbor.raw()) as usize];
                let (input, output) = match resource {
                    Resource::Energy => sides::energy_io(kind, facing, target_face),
                    Resource::Fluid => (
                        matches!(kind, MekKind::Separator | MekKind::Heat),
                        matches!(kind, MekKind::Pump | MekKind::Heat),
                    ),
                    Resource::Gas => (kind == MekKind::Purifier, kind == MekKind::Separator),
                };
                if output {
                    producers.insert(id);
                }
                let pull = mode == 3;
                if input && !pull || output && pull {
                    endpoints.push(machines::grid::Endpoint {
                        owner: id.0,
                        position: [q.x, q.y, q.z],
                        face: target_face,
                        pull,
                    });
                }
            }
        }
        self.grids
            .get_mut(&key.space)
            .ok_or(MachineError::Missing)?
            .set_endpoints(key.network, &endpoints)?;
        self.producers.insert(key, producers.into_iter().collect());
        Ok(())
    }
}
