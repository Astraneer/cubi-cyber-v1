//! One persisted logical clock per occupied page; pausing a dense page rewrites
//! no packet rows. Per-page queues expose one active minimum to the global heap.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageClock {
    pub active: bool,
    pub offset: u64,
    pub paused_since: Tick,
}
impl Default for PageClock {
    fn default() -> Self {
        Self {
            active: true,
            offset: 0,
            paused_since: Tick(0),
        }
    }
}
impl PageClock {
    pub fn validate(self) -> Result<(), MachineError> {
        if self.active && self.paused_since.0 != 0
            || !self.active && (self.paused_since.0 == 0 || self.offset > self.paused_since.0 - 1)
        {
            return Err(MachineError::Encoding);
        }
        Ok(())
    }
    pub fn local_now(self, now: Tick) -> Result<Tick, MachineError> {
        let global = if self.active {
            now.0
        } else {
            self.paused_since.0.saturating_sub(1)
        };
        Ok(Tick(
            global
                .checked_sub(self.offset)
                .ok_or(MachineError::Encoding)?,
        ))
    }
    pub fn global_tick(self, local: Tick) -> Result<Tick, MachineError> {
        Ok(Tick(
            local
                .0
                .checked_add(self.offset)
                .ok_or(MachineError::Overflow)?,
        ))
    }
    pub fn change(self, active: bool, now: Tick) -> Result<Self, MachineError> {
        self.validate()?;
        if self.active == active {
            return Ok(self);
        }
        let out = if active {
            Self {
                active: true,
                offset: self
                    .offset
                    .checked_add(now.0.saturating_sub(self.paused_since.0))
                    .ok_or(MachineError::Overflow)?,
                paused_since: Tick(0),
            }
        } else {
            Self {
                active: false,
                offset: self.offset,
                paused_since: Tick(now.0.max(1)),
            }
        };
        out.validate()?;
        Ok(out)
    }
    fn fresh(active: bool, now: Tick) -> Self {
        if active {
            Self::default()
        } else {
            Self {
                active: false,
                offset: 0,
                paused_since: Tick(now.0.max(1)),
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockRecord {
    pub page: PageId,
    pub clock: PageClock,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockPatch {
    pub page: PageId,
    pub before: Option<PageClock>,
    pub after: Option<PageClock>,
}
fn packet_page(p: &Packet) -> Result<PageId, MachineError> {
    Ok(p.at().ok_or(MachineError::Encoding)?.page())
}
pub(super) fn affected(delta: &TransportDelta) -> Result<BTreeSet<PageId>, MachineError> {
    let mut pages: BTreeSet<_> = delta.clocks.iter().map(|p| p.page).collect();
    for patch in &delta.packets {
        for p in patch.before.iter().chain(patch.after.iter()) {
            pages.insert(packet_page(p)?);
        }
    }
    for patch in &delta.pulls {
        for p in patch.before.iter().chain(patch.after.iter()) {
            pages.insert(p.position.page());
        }
    }
    Ok(pages)
}
impl TransportOwner {
    fn projected_counts(
        &self,
        delta: &TransportDelta,
    ) -> Result<BTreeMap<PageId, usize>, MachineError> {
        let mut counts: BTreeMap<PageId, i64> = delta
            .clocks
            .iter()
            .map(|p| {
                (
                    p.page,
                    self.queues.get(&p.page).map_or(0, |q| q.len()) as i64,
                )
            })
            .collect();
        let mut adjust = |page: PageId, change: i64| {
            *counts
                .entry(page)
                .or_insert_with(|| self.queues.get(&page).map_or(0, |q| q.len()) as i64) += change;
        };
        for patch in &delta.packets {
            if let Some(p) = &patch.before {
                adjust(packet_page(p)?, -1);
            }
            if let Some(p) = &patch.after {
                adjust(packet_page(p)?, 1);
            }
        }
        for patch in &delta.pulls {
            if let Some(p) = patch.before {
                adjust(p.position.page(), -1);
            }
            if let Some(p) = patch.after {
                adjust(p.position.page(), 1);
            }
        }
        counts
            .into_iter()
            .map(|(page, count)| {
                Ok((
                    page,
                    usize::try_from(count).map_err(|_| MachineError::Encoding)?,
                ))
            })
            .collect()
    }
    pub(super) fn finish_clocks(&self, delta: &mut TransportDelta) -> Result<(), MachineError> {
        let mut clocks: BTreeMap<_, _> =
            delta.clocks.iter().copied().map(|p| (p.page, p)).collect();
        for (page, count) in self.projected_counts(delta)? {
            let before = self.clocks.get(&page).copied();
            let after = clocks.get(&page).map(|p| p.after).unwrap_or(before);
            if count == 0 {
                if before.is_some() {
                    clocks.insert(
                        page,
                        ClockPatch {
                            page,
                            before,
                            after: None,
                        },
                    );
                } else {
                    clocks.remove(&page);
                }
            } else if after.is_none() {
                clocks.insert(
                    page,
                    ClockPatch {
                        page,
                        before,
                        after: Some(PageClock::default()),
                    },
                );
            }
        }
        delta.clocks = clocks
            .into_values()
            .filter(|p| p.before != p.after)
            .collect();
        Ok(())
    }
    pub(super) fn validate_clocks(&self, delta: &TransportDelta) -> Result<i64, MachineError> {
        let mut changes = BTreeMap::new();
        let mut bytes = 0i64;
        for patch in &delta.clocks {
            if changes.insert(patch.page, patch.after).is_some()
                || self.clocks.get(&patch.page).copied() != patch.before
            {
                return Err(MachineError::Stale);
            }
            if let Some(after) = patch.after {
                after.validate()?;
            }
            if patch.before.is_some() {
                bytes -= 48;
            }
            if patch.after.is_some() {
                bytes += 48;
            }
        }
        let mut latest = BTreeMap::<PageId, Tick>::new();
        for p in delta.packets.iter().filter_map(|p| p.after.as_ref()) {
            let page = packet_page(p)?;
            latest
                .entry(page)
                .and_modify(|t| *t = (*t).max(p.due))
                .or_insert(p.due);
        }
        for p in delta.pulls.iter().filter_map(|p| p.after) {
            let page = p.position.page();
            latest
                .entry(page)
                .and_modify(|t| *t = (*t).max(p.due))
                .or_insert(p.due);
        }
        for (page, count) in self.projected_counts(delta)? {
            let clock = changes
                .get(&page)
                .copied()
                .unwrap_or_else(|| self.clocks.get(&page).copied());
            if (count == 0) != clock.is_none() {
                return Err(MachineError::Encoding);
            }
            if let Some(clock) = clock
                && clock.active
            {
                let mut last = self
                    .queues
                    .get(&page)
                    .and_then(|q| q.last_key_value().map(|(key, _)| key.0))
                    .unwrap_or(Tick(0));
                if let Some(&new) = latest.get(&page) {
                    last = last.max(new);
                }
                clock.global_tick(last)?;
            }
        }
        for patch in &delta.packets {
            if let Some((before, after)) = patch.before.as_ref().zip(patch.after.as_ref()) {
                // Pages have independent logical clocks. Entering a dormant
                // page can legitimately move from local time 19 to time 9.
                if packet_page(before)? == packet_page(after)? && before.last_tick > after.last_tick
                {
                    return Err(MachineError::Stale);
                }
            }
        }
        Ok(bytes)
    }
    pub(super) fn refresh_page(&mut self, page: PageId) -> Result<(), MachineError> {
        if let Some(key) = self.page_min.remove(&page) {
            self.due_pages.remove(&(key.0, key.1, key.2, page));
        }
        if let Some(clock) = self.clocks.get(&page).filter(|c| c.active)
            && let Some((key, _)) = self.queues.get(&page).and_then(BTreeMap::first_key_value)
        {
            let global = clock.global_tick(key.0)?;
            let key = (global, key.1, key.2);
            self.page_min.insert(page, key);
            self.due_pages.insert((key.0, key.1, key.2, page), ());
        }
        Ok(())
    }
    pub(in crate::machines) fn activity_patches(
        &self,
        changes: &[(PageId, bool)],
        now: Tick,
    ) -> Result<BTreeMap<PageId, ClockPatch>, MachineError> {
        if changes.len() > 131_072 {
            return Err(MachineError::Bounds);
        }
        let mut seen = BTreeSet::new();
        let mut out = BTreeMap::new();
        for &(page, active) in changes {
            if !seen.insert(page) {
                return Err(MachineError::Encoding);
            }
            if let Some(&before) = self.clocks.get(&page) {
                let after = before.change(active, now)?;
                if before != after {
                    out.insert(
                        page,
                        ClockPatch {
                            page,
                            before: Some(before),
                            after: Some(after),
                        },
                    );
                }
            }
        }
        Ok(out)
    }
    pub(in crate::machines) fn agenda(
        &self,
        overlay: &Overlay,
        now: Tick,
        limit: usize,
    ) -> Result<BTreeMap<(u8, u64), TransitEvent>, MachineError> {
        if limit == 0 {
            return Ok(BTreeMap::new());
        }
        let mut heap = BTreeSet::new();
        for (&key, _) in self
            .due_pages
            .range(
                ..=(
                    now,
                    u8::MAX,
                    u64::MAX,
                    PageId {
                        space: SpaceId(u32::MAX),
                        x: i32::MAX,
                        y: i32::MAX,
                        z: i32::MAX,
                    },
                ),
            )
            .take(limit.saturating_add(overlay.clocks.len()))
        {
            if !overlay.clocks.contains_key(&key.3) {
                heap.insert(key);
            }
        }
        for patch in overlay.clocks.values() {
            if let Some(clock) = patch.after.filter(|c| c.active)
                && let Some((key, _)) = self
                    .queues
                    .get(&patch.page)
                    .and_then(BTreeMap::first_key_value)
            {
                let due = clock.global_tick(key.0)?;
                if due <= now {
                    heap.insert((due, key.1, key.2, patch.page));
                }
            }
        }
        let mut out = BTreeMap::new();
        while out.len() < limit {
            let Some((global, phase, order, page)) = heap.pop_first() else {
                break;
            };
            let clock = overlay
                .clocks
                .get(&page)
                .map(|p| p.after)
                .unwrap_or_else(|| self.clocks.get(&page).copied())
                .ok_or(MachineError::Encoding)?;
            let local = Tick(
                global
                    .0
                    .checked_sub(clock.offset)
                    .ok_or(MachineError::Encoding)?,
            );
            let queue = self.queues.get(&page).ok_or(MachineError::Encoding)?;
            let event = *queue
                .get(&(local, phase, order))
                .ok_or(MachineError::Encoding)?;
            out.insert((phase, order), event);
            if let Some((key, _)) = queue
                .range((
                    std::ops::Bound::Excluded((local, phase, order)),
                    std::ops::Bound::Unbounded,
                ))
                .next()
            {
                let due = clock.global_tick(key.0)?;
                if due <= now {
                    heap.insert((due, key.1, key.2, page));
                }
            }
        }
        Ok(out)
    }
    /// Check the detached post-transit queues without cloning or walking cold
    /// packets. At most the changed rows plus one unchanged due row are read.
    pub(in crate::machines) fn has_due_projected(
        &self,
        overlay: &Overlay,
        now: Tick,
    ) -> Result<bool, MachineError> {
        let due = |position: BlockPos, tick: Tick| -> Result<bool, MachineError> {
            let clock = overlay
                .clocks
                .get(&position.page())
                .map(|p| p.after)
                .unwrap_or_else(|| self.clocks.get(&position.page()).copied())
                .ok_or(MachineError::Encoding)?;
            Ok(clock.active && tick <= clock.local_now(now)?)
        };
        let limit = overlay
            .packets
            .len()
            .saturating_add(overlay.pulls.len())
            .saturating_add(1);
        for event in self.agenda(overlay, now, limit)?.into_values() {
            match event {
                TransitEvent::Packet(id) => {
                    if let Some(p) = overlay
                        .packets
                        .get(&id)
                        .map(|p| p.after.as_ref())
                        .unwrap_or_else(|| self.packets.get(&id))
                        && due(p.at().ok_or(MachineError::Encoding)?, p.due)?
                    {
                        return Ok(true);
                    }
                }
                TransitEvent::Pull(position) => {
                    if let Some(p) = overlay
                        .pulls
                        .get(&position)
                        .map(|p| p.after)
                        .unwrap_or_else(|| self.pulls.get(&position).copied())
                        && due(position, p.due)?
                    {
                        return Ok(true);
                    }
                }
            }
        }
        for p in overlay.packets.values().filter_map(|p| p.after.as_ref()) {
            if due(p.at().ok_or(MachineError::Encoding)?, p.due)? {
                return Ok(true);
            }
        }
        for p in overlay.pulls.values().filter_map(|p| p.after) {
            if due(p.position, p.due)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
impl crate::machines::view::Stage<'_> {
    pub(super) fn page_active(&mut self, page: PageId) -> Result<bool, MachineError> {
        let clock = self.ensure_clock(page, false)?;
        if !clock.active {
            return Ok(false);
        }
        if self
            .owner
            .active
            .is_none_or(|active| active(page_origin(page)))
        {
            return Ok(true);
        }
        let after = clock.change(false, self.owner.now)?;
        self.local
            .clocks
            .entry(page)
            .and_modify(|p| p.after = Some(after))
            .or_insert(ClockPatch {
                page,
                before: Some(clock),
                after: Some(after),
            });
        Ok(false)
    }
    pub(super) fn page_clock(&self, page: PageId) -> PageClock {
        self.local
            .clocks
            .get(&page)
            .or_else(|| self.parent.clocks.get(&page))
            .map(|p| p.after)
            .unwrap_or_else(|| self.owner.transport.clocks.get(&page).copied())
            .unwrap_or_else(|| {
                PageClock::fresh(
                    self.owner
                        .active
                        .is_none_or(|active| active(page_origin(page))),
                    self.owner.now,
                )
            })
    }
    pub(super) fn ensure_clock(
        &mut self,
        page: PageId,
        pause_next: bool,
    ) -> Result<PageClock, MachineError> {
        if let Some(clock) = self
            .local
            .clocks
            .get(&page)
            .or_else(|| self.parent.clocks.get(&page))
            .map(|p| p.after)
            .unwrap_or_else(|| self.owner.transport.clocks.get(&page).copied())
        {
            return Ok(clock);
        }
        let active = self
            .owner
            .active
            .is_none_or(|active| active(page_origin(page)));
        let clock = PageClock::fresh(
            active,
            if pause_next {
                Tick(self.owner.now.0.saturating_add(1))
            } else {
                self.owner.now
            },
        );
        clock.validate()?;
        self.local.clocks.insert(
            page,
            ClockPatch {
                page,
                before: None,
                after: Some(clock),
            },
        );
        Ok(clock)
    }
    pub(super) fn local_now(&self, page: PageId) -> Result<Tick, MachineError> {
        self.page_clock(page).local_now(self.owner.now)
    }
}
fn page_origin(page: PageId) -> BlockPos {
    BlockPos {
        space: page.space,
        x: page.x.saturating_mul(16),
        y: page.y.saturating_mul(16),
        z: page.z.saturating_mul(16),
    }
}
impl MachineRuntime {
    pub fn activity_pages(&self) -> impl Iterator<Item = PageId> + '_ {
        self.machine_pages.keys().copied().chain(
            self.transport
                .clocks
                .keys()
                .filter(|p| !self.machine_pages.contains_key(p))
                .copied(),
        )
    }
    pub fn page_clock(&self, page: PageId) -> Option<PageClock> {
        self.transport.clocks.get(&page).copied()
    }
    pub fn packet_epoch(&self, id: u64, now: Tick) -> Result<(Tick, bool), MachineError> {
        let packet = self.packet(id).ok_or(MachineError::Missing)?;
        let page = packet.at().ok_or(MachineError::Encoding)?.page();
        let clock = self
            .transport
            .clocks
            .get(&page)
            .ok_or(MachineError::Encoding)?;
        Ok((
            if clock.active {
                clock.global_tick(packet.last_tick)?
            } else {
                now
            },
            clock.active,
        ))
    }
    pub fn extend_page_activity(
        &self,
        mut delta: RuntimeDelta,
        changes: &[(PageId, bool)],
        now: Tick,
    ) -> Result<RuntimeDelta, MachineError> {
        self.validate_delta(&delta)?;
        let mut transport = delta
            .transport
            .take()
            .unwrap_or_else(|| self.transport.delta());
        let mut patches: BTreeMap<_, _> =
            transport.clocks.into_iter().map(|p| (p.page, p)).collect();
        for (page, patch) in self.transport.activity_patches(changes, now)? {
            let after = patches
                .get(&page)
                .and_then(|p| p.after)
                .unwrap_or(patch.before.expect("existing clock"))
                .change(patch.after.expect("activity clock").active, now)?;
            patches.insert(
                page,
                ClockPatch {
                    page,
                    before: patch.before,
                    after: Some(after),
                },
            );
        }
        transport.clocks = patches
            .into_values()
            .filter(|p| p.before != p.after)
            .collect();
        self.transport.finish_clocks(&mut transport)?;
        if !transport.is_empty() {
            delta.transport = Some(transport);
        }
        self.validate_delta(&delta)?;
        Ok(delta)
    }
}
