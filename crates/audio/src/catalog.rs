//! Original CC0/CC-BY default sounds. Decode lazily; compressed source and credits
//! are embedded in native/Web builds and never fetched from third-party hosts.
use crate::{AudioError, Category, ClipId, EmitterId, Mixer, Pcm, VoiceRequest};
use core_types::DeterministicRng;
use std::collections::BTreeMap;
mod data {
    include!("default_catalog.rs");
}
pub const CREDITS: &str = include_str!("../../../donor/web/sounds/CREDITS.md");
pub struct Catalog {
    loaded: BTreeMap<&'static str, ClipId>,
    local_loaded: BTreeMap<(String, u64, u64), ClipId>,
}
impl Default for Catalog {
    fn default() -> Self {
        Self::new()
    }
}
impl Catalog {
    pub fn new() -> Self {
        Self {
            loaded: BTreeMap::new(),
            local_loaded: BTreeMap::new(),
        }
    }
    pub fn events() -> impl Iterator<Item = &'static str> {
        data::EVENTS.iter().map(|(name, _)| *name)
    }
    pub fn files() -> impl Iterator<Item = (&'static str, &'static [u8])> {
        data::FILES.iter().copied()
    }
    /// Loading-phase preparation. Subsequent requests for default events only
    /// select existing IDs, and never decode a compressed stream in a frame.
    pub fn prepare_all(&mut self, mixer: &mut Mixer) -> Result<(), AudioError> {
        for (path, bytes) in Self::files() {
            if !self.loaded.contains_key(path) {
                let clip = mixer.add_clip(Pcm::decode_ogg(bytes)?)?;
                self.loaded.insert(path, clip);
            }
        }
        Ok(())
    }
    pub fn compressed(path: &str) -> Option<&'static [u8]> {
        data::FILES
            .iter()
            .find_map(|(name, data)| (*name == path).then_some(*data))
    }
    /// Invoke while preparing an imported pack. Replacing a pack should rebuild
    /// its audio bank so old private clip revisions do not occupy the quota.
    pub fn prepare_with_local(
        &mut self,
        mixer: &mut Mixer,
        resources: &crate::resources::LocalResources,
        event: &str,
        variant: u64,
        index: Option<&str>,
    ) -> Result<ClipId, AudioError> {
        let key = (event.to_owned(), variant, resources.revision());
        if let Some(id) = self.local_loaded.get(&key) {
            return Ok(*id);
        }
        if let Some(bytes) = resources
            .sound_for_event(event, variant, index)
            .map_err(|_| AudioError::Decode)?
        {
            let clip = mixer.add_clip(Pcm::decode_ogg(bytes)?)?;
            self.local_loaded.insert(key, clip);
            Ok(clip)
        } else {
            self.prepare(mixer, event, variant)
        }
    }
    pub fn prepare(
        &mut self,
        mixer: &mut Mixer,
        event: &str,
        seed: u64,
    ) -> Result<ClipId, AudioError> {
        let files = data::EVENTS
            .iter()
            .find_map(|(name, files)| (*name == event).then_some(*files))
            .ok_or(AudioError::MissingClip)?;
        let mut rng = DeterministicRng::from_seed(seed);
        let path = files[rng
            .below(files.len() as u64)
            .ok_or(AudioError::MissingClip)? as usize];
        if let Some(id) = self.loaded.get(path) {
            return Ok(*id);
        }
        let id = mixer.add_clip(Pcm::decode_ogg(
            Self::compressed(path).ok_or(AudioError::MissingClip)?,
        )?)?;
        self.loaded.insert(path, id);
        Ok(id)
    }
    pub fn request(
        &mut self,
        mixer: &mut Mixer,
        event: &str,
        emitter: EmitterId,
        position: Option<[f64; 3]>,
        seed: u64,
    ) -> Result<VoiceRequest, AudioError> {
        let clip = self.prepare(mixer, event, seed)?;
        Ok(Self::voice_request(clip, event, emitter, position, seed))
    }
    /// Loading must have called `prepare_all`. This path cannot decode or grow
    /// the bank, and is suitable for real game-event handling.
    pub fn request_prepared(
        &self,
        event: &str,
        emitter: EmitterId,
        position: Option<[f64; 3]>,
        seed: u64,
    ) -> Result<VoiceRequest, AudioError> {
        let files = data::EVENTS
            .iter()
            .find_map(|(name, files)| (*name == event).then_some(*files))
            .ok_or(AudioError::MissingClip)?;
        let mut rng = DeterministicRng::from_seed(seed);
        let path = files[rng
            .below(files.len() as u64)
            .ok_or(AudioError::MissingClip)? as usize];
        let clip = *self.loaded.get(path).ok_or(AudioError::MissingClip)?;
        Ok(Self::voice_request(clip, event, emitter, position, seed))
    }
    fn voice_request(
        clip: ClipId,
        event: &str,
        emitter: EmitterId,
        position: Option<[f64; 3]>,
        seed: u64,
    ) -> VoiceRequest {
        let category = if event.starts_with("ui.") {
            Category::Ui
        } else if event.starts_with("machine.") {
            Category::Machines
        } else if event.starts_with("player.") {
            Category::Players
        } else if event.starts_with("entity.") {
            Category::Entities
        } else if event.starts_with("ambient.") {
            Category::Ambient
        } else {
            Category::Blocks
        };
        let mut request = VoiceRequest::local(clip, emitter, category);
        request.position = position;
        request.gain = if event.ends_with(".step") {
            0.15
        } else if event.ends_with(".hit") {
            0.25
        } else {
            1.0
        };
        let mut rng = DeterministicRng::from_key(seed, 12, emitter.0, 0);
        request.pitch = 0.95 + (rng.below(1001).unwrap() as f32 / 1000.0) * 0.1;
        request.looping = event.contains("loop");
        request.priority = if request.looping { 32 } else { 128 };
        request
    }
}
