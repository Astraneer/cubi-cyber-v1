//! Bounded transient public events. The reliable control stream is ordered by
//! server tick; late events and duplicates never replay a historical sound.
use net::audio_cue::Cue;
pub const CAPACITY: usize = 64;
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub revision: u64,
    pub cue: Cue,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub duplicate: u64,
    pub stale: u64,
    pub overflow: u64,
    pub overwritten: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    RevisionOverflow,
}
pub struct AudioCues {
    entries: [Option<Entry>; CAPACITY],
    head: usize,
    len: usize,
    revision: u64,
    sequence: u64,
    actors: [u64; CAPACITY],
    actors_len: usize,
    counts: Counts,
}
impl Default for AudioCues {
    fn default() -> Self {
        Self {
            entries: [None; CAPACITY],
            head: 0,
            len: 0,
            revision: 0,
            sequence: 0,
            actors: [0; CAPACITY],
            actors_len: 0,
            counts: Counts::default(),
        }
    }
}
impl AudioCues {
    pub fn push(&mut self, cue: Cue) -> Result<bool, Error> {
        cue.validate().map_err(|_| Error::Invalid)?;
        if cue.sequence < self.sequence {
            self.counts.stale = self.counts.stale.saturating_add(1);
            return Ok(false);
        }
        if cue.sequence == self.sequence {
            if self.actors[..self.actors_len].contains(&cue.actor) {
                self.counts.duplicate = self.counts.duplicate.saturating_add(1);
                return Ok(false);
            }
            if self.actors_len == CAPACITY {
                self.counts.overflow = self.counts.overflow.saturating_add(1);
                return Ok(false);
            }
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(Error::RevisionOverflow)?;
        if cue.sequence > self.sequence {
            self.sequence = cue.sequence;
            self.actors_len = 0;
        }
        self.actors[self.actors_len] = cue.actor;
        self.actors_len += 1;
        if self.len == CAPACITY {
            self.head = (self.head + 1) % CAPACITY;
            self.len -= 1;
            self.counts.overwritten = self.counts.overwritten.saturating_add(1);
        }
        self.entries[(self.head + self.len) % CAPACITY] = Some(Entry { revision, cue });
        self.len += 1;
        self.revision = revision;
        Ok(true)
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn counts(&self) -> Counts {
        self.counts
    }
    pub fn after(&self, revision: u64) -> impl Iterator<Item = &Entry> {
        (0..self.len)
            .filter_map(move |offset| self.entries[(self.head + offset) % CAPACITY].as_ref())
            .filter(move |entry| entry.revision > revision)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use core_types::{SpaceId, Tick};
    fn cue(actor: u64, sequence: u64) -> Cue {
        Cue {
            kind: net::audio_cue::Kind::ChickenEgg,
            actor,
            sequence,
            tick: Tick(sequence),
            space: SpaceId(0),
            position: [1., 2., 3.],
        }
    }
    #[test]
    fn codec_events_deduplicate_and_burst_retention_stays_bounded() {
        let mut cache = AudioCues::default();
        let first = Cue::decode(&cue(1, 10).encode().unwrap()).unwrap();
        assert!(cache.push(first).unwrap());
        assert!(!cache.push(first).unwrap());
        assert!(!cache.push(cue(1, 9)).unwrap());
        for actor in 2..=64 {
            assert!(cache.push(cue(actor, 10)).unwrap());
        }
        assert!(!cache.push(cue(65, 10)).unwrap());
        assert_eq!(cache.after(0).count(), 64);
        assert!(cache.push(cue(1, 11)).unwrap());
        assert_eq!(cache.after(0).next().unwrap().revision, 2);
        assert_eq!(cache.after(64).next().unwrap().cue.sequence, 11);
        assert_eq!(
            cache.counts(),
            Counts {
                duplicate: 1,
                stale: 1,
                overflow: 1,
                overwritten: 1
            }
        );
        let revision = cache.revision();
        assert_eq!(cache.push(Cue { actor: 0, ..first }), Err(Error::Invalid));
        assert_eq!(cache.revision(), revision);
    }
}
