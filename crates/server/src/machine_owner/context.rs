//! Captured tick participants outlive network sessions. The bounded durable
//! kind4 context names actors; their exact states already share its opening WAL.
use super::{
    actors::{ActorMut, ActorRef, TickActor},
    *,
};
pub(crate) struct TickContext {
    pub(crate) tick: Tick,
    pub(crate) pages: BTreeSet<PageId>,
    observers: BTreeSet<PlayerId>,
    offline: BTreeMap<PlayerId, TickActor>,
}
impl Server {
    pub(crate) fn restore_tick_context(&mut self) -> Result<(), ServerError> {
        if self.runtimes.machines.owner_context_tick() != Some(self.tick) {
            self.tick_context = None;
            return Ok(());
        }
        if self
            .tick_context
            .as_ref()
            .is_none_or(|c| c.tick != self.tick)
        {
            let pages = self
                .runtimes
                .machines
                .owner_active_pages()
                .ok_or(ServerError::Invalid("captured tick activity is missing"))?
                .iter()
                .copied()
                .collect();
            let observers = self
                .runtimes
                .machines
                .owner_observers()
                .ok_or(ServerError::Invalid("captured tick actors are missing"))?
                .iter()
                .copied()
                .collect();
            self.tick_context = Some(TickContext {
                tick: self.tick,
                pages,
                observers,
                offline: BTreeMap::new(),
            });
            self.runtimes.install_frozen_sections(
                &self.tick_context.as_ref().expect("context restored").pages,
            );
        }
        let context = self.tick_context.as_ref().expect("context restored");
        let missing: Vec<_> = context
            .observers
            .iter()
            .filter(|id| {
                !context.offline.contains_key(id)
                    && !self.peers.values().any(|p| p.ready && p.identity == **id)
            })
            .copied()
            .collect();
        for id in missing {
            let identity = self
                .identities
                .get(&id)
                .ok_or(ServerError::Invalid("captured actor identity is missing"))?;
            let saved = self
                .saved_players
                .get(&id)
                .ok_or(ServerError::Invalid("captured actor state is missing"))?;
            let actor =
                TickActor::restore(saved, self.config.server_debug && identity.role.can_admin())?;
            self.tick_context
                .as_mut()
                .expect("context restored")
                .offline
                .insert(id, actor);
        }
        Ok(())
    }
    fn tick_actor_refs_unsorted(&self) -> impl Iterator<Item = ActorRef<'_>> {
        let context = self.tick_context.as_ref().filter(|c| c.tick == self.tick);
        self.peers
            .values()
            .filter(move |p| {
                p.ready
                    && context.is_none_or(|c| {
                        c.observers.contains(&p.identity) && !c.offline.contains_key(&p.identity)
                    })
            })
            .map(ActorRef::from)
            .chain(
                context
                    .into_iter()
                    .flat_map(|c| c.offline.values())
                    .map(TickActor::as_ref),
            )
    }
    pub(crate) fn tick_actor_refs(&self) -> impl Iterator<Item = ActorRef<'_>> {
        // Canonical identity order also makes pickup ties and spawn-input hashes
        // independent of reconnect connection numbers or offline/live storage.
        let mut actors = [None; MAX_CONNECTIONS];
        let mut count = 0;
        for actor in self.tick_actor_refs_unsorted() {
            // Live and restored actors are disjoint and retain the same
            // bounded configured player population.
            actors[count] = Some(actor);
            count += 1;
        }
        actors[..count].sort_unstable_by_key(|p| p.expect("filled actor prefix").identity);
        actors.into_iter().take(count).flatten()
    }
    pub(crate) fn any_tick_actor(&self, predicate: impl FnMut(ActorRef<'_>) -> bool) -> bool {
        self.tick_actor_refs_unsorted().any(predicate)
    }
    pub(crate) fn tick_actor_ref(&self, id: PlayerId) -> Option<ActorRef<'_>> {
        self.tick_actor_refs_unsorted().find(|p| p.identity == id)
    }
    pub(crate) fn tick_actor_mut(&mut self, id: PlayerId) -> Option<ActorMut<'_>> {
        let context = self.tick_context.as_ref().filter(|c| c.tick == self.tick);
        if context.is_some_and(|c| !c.observers.contains(&id)) {
            return None;
        }
        if context.is_some_and(|c| c.offline.contains_key(&id)) {
            return self
                .tick_context
                .as_mut()?
                .offline
                .get_mut(&id)
                .map(TickActor::as_mut);
        }
        self.peers
            .values_mut()
            .find(|p| p.ready && p.identity == id)
            .map(ActorMut::from)
    }
    pub(crate) fn record_tick_actor(&mut self, saved: SavedPlayer, batch: &mut Batch) {
        self.saved_players
            .insert(saved.state.identity, saved.clone());
        if let Some(old) = batch
            .players
            .iter_mut()
            .find(|p| p.state.identity == saved.state.identity)
        {
            *old = saved;
        } else {
            batch.players.push(saved);
        }
    }
    pub(crate) fn capture_yield_context(&mut self, batch: &mut Batch) -> Result<(), ServerError> {
        if !self.mob_owner.due || self.runtimes.machines.owner_context_tick() == Some(self.tick) {
            return Ok(());
        }
        let observers: BTreeSet<_> = self
            .peers
            .values()
            .filter(|p| p.ready)
            .map(|p| p.identity)
            .collect();
        let existing = batch
            .runtimes
            .iter()
            .position(|r| r.kind == save::RUNTIME_MACHINES);
        let mut delta = if let Some(index) = existing {
            machines::RuntimeDelta::decode(
                &batch.runtimes[index].payload,
                machines::Config::default(),
            )
            .map_err(|_| ServerError::Invalid("captured tick pending machine codec"))?
        } else {
            self.runtimes
                .machines
                .noop_delta()
                .map_err(|_| ServerError::Invalid("captured tick boundary"))?
        };
        delta
            .capture_context(
                self.tick,
                &self.runtimes.active_sections().collect(),
                &observers,
            )
            .map_err(|_| ServerError::Invalid("captured tick context bound"))?;
        let record = save::RuntimeRecord {
            kind: save::RUNTIME_MACHINES,
            payload: delta
                .encode()
                .map_err(|_| ServerError::Invalid("captured tick context codec"))?,
        };
        if let Some(index) = existing {
            batch.runtimes[index] = record;
        } else {
            batch.runtimes.push(record);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
