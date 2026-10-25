//! Bounded external component changes join the same tick revision/WAL record.
use super::*;
impl MachineRuntime {
    pub fn extend_registration(
        &self,
        mut delta: RuntimeDelta,
        world: &World,
        components: impl IntoIterator<Item = ComponentId>,
        now: Tick,
    ) -> Result<RuntimeDelta, MachineError> {
        self.validate_delta(&delta)?;
        let mut ids = BTreeSet::new();
        for id in components {
            ids.insert(id);
            if ids.len() > self.config.machines {
                return Err(MachineError::Bounds);
            }
        }
        let mut metadata: BTreeMap<_, _> = delta
            .machines
            .into_iter()
            .map(|p| Ok((p.after.or(p.before).ok_or(MachineError::Encoding)?.id, p)))
            .collect::<Result<_, MachineError>>()?;
        let mut queue: BTreeMap<_, _> = delta
            .queue
            .into_iter()
            .map(|p| Ok((p.after.or(p.before).ok_or(MachineError::Encoding)?.event, p)))
            .collect::<Result<_, MachineError>>()?;
        for id in ids {
            let before = self.machine_meta(id);
            let effective = metadata.get(&id).map(|p| p.after).unwrap_or(before);
            let record = world
                .component(id)
                .filter(|record| record.data.kind == MACHINE_COMPONENT);
            let event = Event::Machine(id);
            if let Some(record) = record {
                let machine =
                    Machine::decode(&record.data.payload).map_err(|_| MachineError::Encoding)?;
                let CellRead::Known(cell) = world.read(record.data.anchor) else {
                    return Err(MachineError::Unknown);
                };
                if !matches!(blocks::bdef(cell.id()).be,Some(BeKind::Mek(kind)) if kind==machine.kind)
                {
                    return Err(MachineError::Component);
                }
                let meta = if let Some(meta) = effective {
                    if meta.anchor != record.data.anchor {
                        return Err(MachineError::Component);
                    }
                    // The owner has already ticked this exact external input.
                    // Its deliberate sleep must survive its own component write.
                    if metadata.contains_key(&id) && meta.last_tick == now {
                        continue;
                    }
                    MachineMeta {
                        active: true,
                        last_tick: if meta.active {
                            meta.last_tick
                        } else {
                            Tick(now.0.saturating_sub(1))
                        },
                        ..meta
                    }
                } else {
                    let order = delta.next_order_after;
                    delta.next_order_after = order.checked_add(1).ok_or(MachineError::Overflow)?;
                    MachineMeta {
                        id,
                        anchor: record.data.anchor,
                        order,
                        last_tick: Tick(now.0.saturating_sub(1)),
                        active: true,
                    }
                };
                if effective != Some(meta) {
                    metadata.insert(
                        id,
                        MetaPatch {
                            before,
                            after: Some(meta),
                        },
                    );
                }
                let due = Tick(now.0.checked_add(1).ok_or(MachineError::Overflow)?);
                let old = queue
                    .get(&event)
                    .map(|p| p.after)
                    .unwrap_or_else(|| self.pending(event));
                let next = old.map_or(
                    Pending {
                        event,
                        due,
                        first_due: due,
                        order: meta.order,
                    },
                    |old| Pending {
                        due: old.due.min(due),
                        first_due: old.first_due.min(due),
                        ..old
                    },
                );
                let source = self.pending(event);
                if source != Some(next) {
                    queue.insert(
                        event,
                        QueuePatch {
                            before: source,
                            after: Some(next),
                        },
                    );
                }
            } else if effective.is_some() {
                metadata.insert(
                    id,
                    MetaPatch {
                        before,
                        after: None,
                    },
                );
                let source = self.pending(event);
                if source.is_some() || queue.contains_key(&event) {
                    queue.insert(
                        event,
                        QueuePatch {
                            before: source,
                            after: None,
                        },
                    );
                }
            }
        }
        delta.machines = metadata
            .into_values()
            .filter(|p| p.before != p.after)
            .collect();
        delta.queue = queue
            .into_values()
            .filter(|p| p.before != p.after)
            .collect();
        self.prune_retired_endpoints(&mut delta)?;
        self.validate_delta(&delta)?;
        Ok(delta)
    }

    fn prune_retired_endpoints(&self, delta: &mut RuntimeDelta) -> Result<(), MachineError> {
        let removed: BTreeSet<_> = delta
            .machines
            .iter()
            .filter(|p| p.after.is_none())
            .filter_map(|p| p.before.map(|m| m.id))
            .collect();
        if removed.is_empty() {
            return Ok(());
        }
        let mut affected = BTreeSet::new();
        for meta in delta
            .machines
            .iter()
            .filter(|p| p.after.is_none())
            .filter_map(|p| p.before)
        {
            for face in Face::ALL {
                if let Ok(p) = offset(meta.anchor, face)
                    && let Some(key) = self.network_at(p)
                {
                    affected.insert(key);
                }
            }
        }
        for key in affected {
            let network = self.projected_network(key, delta)?;
            let index = delta.endpoints.iter().position(|p| p.key == key);
            let before = network.endpoints().to_vec();
            let mut after =
                index.map_or_else(|| before.clone(), |i| delta.endpoints[i].after.clone());
            after.retain(|e| !removed.contains(&ComponentId(e.owner)));
            if let Some(i) = index {
                delta.endpoints[i].after = after;
            } else if before != after {
                delta.endpoints.push(EndpointPatch { key, before, after });
            }
            let index = delta.networks.iter().position(|p| {
                p.after
                    .as_ref()
                    .or(p.before.as_ref())
                    .is_some_and(|m| m.key == key)
            });
            let before = self.network_meta(key);
            let mut after =
                index.map_or_else(|| before.clone(), |i| delta.networks[i].after.clone());
            if let Some(m) = &mut after {
                m.producers.retain(|id| !removed.contains(id));
            }
            if let Some(i) = index {
                delta.networks[i].after = after;
            } else if before != after {
                delta.networks.push(NetworkPatch { before, after });
            }
        }
        Ok(())
    }
}
