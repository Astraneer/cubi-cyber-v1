//! Global render ownership over independently confirmed page protocols. A move
//! can arrive before its old-page removal; only the newest event owns the ID.
//! Tombstones live only while another protocol page still references that ID.
use super::*;
#[derive(Clone, Copy)]
struct Winner {
    references: usize,
    page: Option<PageId>,
    tick: Tick,
    pose: Option<Pose>,
}
pub struct PacketScene {
    pages: BTreeMap<PageId, PacketPage>,
    winners: BTreeMap<u64, Winner>,
    maximum_pages: usize,
    maximum_rows: usize,
    raw_rows: usize,
    visible: usize,
}
impl PacketScene {
    pub fn new(maximum_pages: usize, maximum_rows: usize) -> Result<Self, Error> {
        if maximum_pages == 0 || maximum_pages > 512 || maximum_rows == 0 || maximum_rows > 2048 {
            return Err(Error::Capacity);
        }
        Ok(Self {
            pages: BTreeMap::new(),
            winners: BTreeMap::new(),
            maximum_pages,
            maximum_rows,
            raw_rows: 0,
            visible: 0,
        })
    }
    pub fn len(&self) -> usize {
        self.visible
    }
    pub fn is_empty(&self) -> bool {
        self.visible == 0
    }
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
    pub fn protocol_rows(&self) -> usize {
        self.raw_rows
    }
    pub fn retained_ids(&self) -> usize {
        self.winners.len()
    }
    pub fn page(&self, page: PageId) -> Option<&PacketPage> {
        self.pages.get(&page)
    }
    pub fn get(&self, id: u64) -> Option<(PageId, Pose)> {
        let winner = self.winners.get(&id)?;
        Some((winner.page?, winner.pose?))
    }
    pub fn apply(&mut self, frame: Frame, certified: Version) -> Result<Apply, Error> {
        let page = frame.page;
        let tick = frame.tick;
        let old = self.pages.get(&page);
        if old.is_none() && self.pages.len() == self.maximum_pages {
            return Err(Error::Capacity);
        }
        let mut next = old.cloned().unwrap_or(PacketPage::new(
            page,
            self.maximum_rows.min(wire::MAX_PACKETS),
        )?);
        let upserts: BTreeMap<_, _> = frame.upserts.iter().map(|&p| (p.id, p)).collect();
        let result = next.apply(frame, certified)?;
        if !matches!(result, Apply::Applied(_)) {
            return Ok(result);
        }
        let raw_rows = self.raw_rows - old.map_or(0, PacketPage::len) + next.len();
        // Reserve one old protocol reference per visible ID while a page move
        // and its independently delivered removal cross in flight.
        if raw_rows > self.maximum_rows.saturating_mul(2) {
            return Err(Error::Capacity);
        }
        let mut affected: BTreeMap<_, _> = upserts.keys().map(|&id| (id, ())).collect();
        if let Some(old) = old {
            affected.extend(
                old.rows
                    .keys()
                    .filter(|id| !next.rows.contains_key(id))
                    .map(|&id| (id, ())),
            );
        }
        let mut patches = Vec::with_capacity(affected.len());
        let mut visible = self.visible;
        let mut retained = self.winners.len();
        for id in affected.into_keys() {
            let before = self.winners.get(&id).copied();
            let had = old.is_some_and(|p| p.rows.contains_key(&id));
            let has = next.rows.contains_key(&id);
            let mut after = before.unwrap_or(Winner {
                references: 0,
                page: None,
                tick: Tick(0),
                pose: None,
            });
            if had && !has {
                after.references = after.references.checked_sub(1).ok_or(Error::Invalid)?;
            }
            if !had && has {
                after.references = after.references.checked_add(1).ok_or(Error::Capacity)?;
            }
            if let Some(&pose) = upserts.get(&id) {
                if tick > after.tick
                    || tick == after.tick
                        && (after.pose.is_none() || after.page.is_none_or(|old| page >= old))
                {
                    after.page = Some(page);
                    after.tick = tick;
                    after.pose = Some(pose);
                }
            } else if had && !has && after.page == Some(page) && tick >= after.tick {
                after.page = None;
                after.tick = tick;
                after.pose = None;
            }
            let after = (after.references != 0).then_some(after);
            retained = retained - usize::from(before.is_some()) + usize::from(after.is_some());
            visible = visible - usize::from(before.is_some_and(|w| w.pose.is_some()))
                + usize::from(after.is_some_and(|w| w.pose.is_some()));
            patches.push((id, after));
        }
        if retained > self.maximum_rows {
            return Err(Error::Capacity);
        }
        // All limits and checked arithmetic precede any scene mutation.
        self.pages.insert(page, next);
        for (id, after) in patches {
            if let Some(after) = after {
                self.winners.insert(id, after);
            } else {
                self.winners.remove(&id);
            }
        }
        self.visible = visible;
        self.raw_rows = raw_rows;
        Ok(result)
    }
    pub fn remove_page(&mut self, page: PageId) -> bool {
        let Some(old) = self.pages.remove(&page) else {
            return false;
        };
        self.raw_rows -= old.len();
        for id in old.rows.keys() {
            if let Some(winner) = self.winners.get_mut(id) {
                winner.references -= 1;
                if winner.page == Some(page) {
                    winner.page = None;
                    winner.pose = None;
                    winner.tick = winner.tick.max(old.tick);
                    self.visible -= 1;
                }
                if winner.references == 0 {
                    self.winners.remove(id);
                }
            }
        }
        true
    }
    pub fn instances(
        &self,
        tick: Tick,
        alpha: f64,
        origin: [i64; 3],
        out: &mut [Instance],
    ) -> Result<usize, Error> {
        if !alpha.is_finite() {
            return Err(Error::Time);
        }
        if out.len() < self.visible {
            return Err(Error::Capacity);
        }
        let mut count = 0;
        for winner in self.winners.values() {
            if let Some((page, pose)) = winner.page.zip(winner.pose) {
                out[count] = instance_for(pose, page, tick, alpha, origin);
                count += 1;
            }
        }
        Ok(count)
    }
}
