//! Allocation-free bounded voice mixing. PCM decoding and device construction
//! happen on loading/control paths, never in the audio callback.
use std::{io::Cursor, sync::Arc};

pub const MAX_VOICES: usize = 64;
pub const MAX_CLIPS: usize = 512;
pub const MAX_DECODED_FRAMES: usize = 4_000_000;
pub const MAX_COMPRESSED_CLIP: usize = 16 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioError {
    Invalid,
    Capacity,
    MissingClip,
    Decode,
    UnsupportedDevice,
    DeviceUnavailable,
    DeviceFailure,
}
impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "audio error: {self:?}")
    }
}
impl std::error::Error for AudioError {}
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Blocks,
    Entities,
    Players,
    Machines,
    Ambient,
    Music,
    Ui,
}
pub const CATEGORIES: usize = 7;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClipId(pub u16);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmitterId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceHandle {
    pub slot: u8,
    pub generation: u64,
}
#[derive(Clone)]
pub struct Pcm {
    samples: Arc<[f32]>,
    rate: u32,
}
impl Pcm {
    pub fn mono(samples: Vec<f32>, rate: u32) -> Result<Self, AudioError> {
        if !(8_000..=192_000).contains(&rate)
            || samples.is_empty()
            || samples.len() > MAX_DECODED_FRAMES
            || samples.iter().any(|v| !v.is_finite() || v.abs() > 1.0)
        {
            return Err(AudioError::Invalid);
        }
        Ok(Self {
            samples: samples.into(),
            rate,
        })
    }
    pub fn decode_ogg(bytes: &[u8]) -> Result<Self, AudioError> {
        if bytes.len() > MAX_COMPRESSED_CLIP {
            return Err(AudioError::Capacity);
        }
        let mut reader = lewton::inside_ogg::OggStreamReader::new(Cursor::new(bytes))
            .map_err(|_| AudioError::Decode)?;
        let channels = usize::from(reader.ident_hdr.audio_channels);
        let rate = reader.ident_hdr.audio_sample_rate;
        if !(1..=2).contains(&channels) || !(8_000..=192_000).contains(&rate) {
            return Err(AudioError::Invalid);
        }
        let mut samples = Vec::new();
        while let Some(packet) = reader
            .read_dec_packet_itl()
            .map_err(|_| AudioError::Decode)?
        {
            if packet.len() % channels != 0
                || samples.len().saturating_add(packet.len() / channels) > MAX_DECODED_FRAMES
            {
                return Err(AudioError::Capacity);
            }
            for frame in packet.chunks_exact(channels) {
                samples.push(
                    frame.iter().map(|v| f32::from(*v) / 32768.0).sum::<f32>() / channels as f32,
                );
            }
        }
        Self::mono(samples, rate)
    }
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
    pub fn rate(&self) -> u32 {
        self.rate
    }
    pub fn bytes(&self) -> usize {
        self.samples.len() * 4
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Listener {
    pub position: [f64; 3],
    pub right: [f32; 3],
}
impl Default for Listener {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            right: [1.0, 0.0, 0.0],
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct VoiceRequest {
    pub clip: ClipId,
    pub emitter: EmitterId,
    pub category: Category,
    pub position: Option<[f64; 3]>,
    pub gain: f32,
    pub pitch: f32,
    pub priority: u8,
    pub looping: bool,
    pub range: f32,
}
impl VoiceRequest {
    pub fn local(clip: ClipId, emitter: EmitterId, category: Category) -> Self {
        Self {
            clip,
            emitter,
            category,
            position: None,
            gain: 1.0,
            pitch: 1.0,
            priority: 128,
            looping: false,
            range: 16.0,
        }
    }
}
#[derive(Clone, Copy)]
struct Voice {
    request: VoiceRequest,
    cursor: f64,
    generation: u64,
    left: f32,
    right: f32,
    stopping: u16,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MixWork {
    pub frames: u64,
    pub voice_samples: u64,
    pub spatial_updates: u64,
    pub peak_voices: usize,
    pub stolen: u64,
    pub rejected: u64,
    pub echo_products: u64,
}
pub struct Mixer {
    rate: u32,
    clips: Vec<Pcm>,
    voices: [Option<Voice>; MAX_VOICES],
    voice_limit: usize,
    listener: Listener,
    master: f32,
    volumes: [f32; CATEGORIES],
    next_generation: u64,
    work: MixWork,
    memory_limit: usize,
    clip_bytes: usize,
    echo: Option<crate::echo::SparseEcho>,
}
impl Mixer {
    pub fn new(rate: u32, voice_limit: usize, memory_limit: usize) -> Result<Self, AudioError> {
        if !(8_000..=192_000).contains(&rate) || voice_limit == 0 || voice_limit > MAX_VOICES {
            return Err(AudioError::Invalid);
        }
        Ok(Self {
            rate,
            clips: Vec::with_capacity(MAX_CLIPS),
            voices: [None; MAX_VOICES],
            voice_limit,
            listener: Listener::default(),
            master: 1.0,
            volumes: [1.0; CATEGORIES],
            next_generation: 1,
            work: MixWork::default(),
            memory_limit,
            clip_bytes: 0,
            echo: None,
        })
    }
    pub fn rate(&self) -> u32 {
        self.rate
    }
    pub fn cavity(&mut self, cavity: crate::echo::Cavity) -> Result<(), AudioError> {
        self.echo = crate::echo::SparseEcho::prepare(cavity, self.rate)?;
        Ok(())
    }
    pub fn add_clip(&mut self, pcm: Pcm) -> Result<ClipId, AudioError> {
        if self.clips.len() == MAX_CLIPS
            || pcm.bytes() > self.memory_limit.saturating_sub(self.clip_bytes)
        {
            return Err(AudioError::Capacity);
        }
        self.clip_bytes += pcm.bytes();
        let id = ClipId(self.clips.len() as u16);
        self.clips.push(pcm);
        Ok(id)
    }
    pub fn clip(&self, id: ClipId) -> Option<&Pcm> {
        self.clips.get(usize::from(id.0))
    }
    pub fn active_voices(&self) -> usize {
        self.voices[..self.voice_limit]
            .iter()
            .filter(|v| v.is_some())
            .count()
    }
    pub fn listener(&mut self, listener: Listener) -> Result<(), AudioError> {
        if listener.position.iter().any(|v| !v.is_finite())
            || listener.right.iter().any(|v| !v.is_finite())
        {
            return Err(AudioError::Invalid);
        }
        let norm = libm::sqrtf(listener.right.iter().map(|v| v * v).sum());
        if norm < 0.001 {
            return Err(AudioError::Invalid);
        }
        self.listener = Listener {
            position: listener.position,
            right: listener.right.map(|v| v / norm),
        };
        self.refresh_gains();
        Ok(())
    }
    pub fn volume(&mut self, category: Option<Category>, gain: f32) -> Result<(), AudioError> {
        if !gain.is_finite() || !(0.0..=1.0).contains(&gain) {
            return Err(AudioError::Invalid);
        }
        if let Some(category) = category {
            self.volumes[category as usize] = gain;
        } else {
            self.master = gain;
        }
        self.refresh_gains();
        Ok(())
    }
    fn gains(&self, request: VoiceRequest) -> [f32; 2] {
        let base = request.gain * self.master * self.volumes[request.category as usize];
        let (pan, attenuation) = if let Some(position) = request.position {
            let delta = std::array::from_fn::<_, 3, _>(|i| {
                (position[i] - self.listener.position[i]) as f32
            });
            let distance = libm::sqrtf(delta.iter().map(|v| v * v).sum());
            let pan = if distance < 0.0001 {
                0.0
            } else {
                delta
                    .iter()
                    .zip(self.listener.right)
                    .map(|(a, b)| a * b)
                    .sum::<f32>()
                    / distance
            };
            (
                pan.clamp(-1.0, 1.0),
                (1.0 - distance / request.range).clamp(0.0, 1.0),
            )
        } else {
            (0.0, 1.0)
        };
        [
            libm::sqrtf((1.0 - pan) * 0.5) * base * attenuation,
            libm::sqrtf((1.0 + pan) * 0.5) * base * attenuation,
        ]
    }
    fn refresh_gains(&mut self) {
        for index in 0..self.voice_limit {
            if let Some(voice) = self.voices[index] {
                let [left, right] = self.gains(voice.request);
                let target = self.voices[index].as_mut().unwrap();
                target.left = left;
                target.right = right;
                self.work.spatial_updates += 1;
            }
        }
    }
    pub fn play(&mut self, request: VoiceRequest) -> Result<VoiceHandle, AudioError> {
        if self.clip(request.clip).is_none() {
            return Err(AudioError::MissingClip);
        }
        if !request.gain.is_finite()
            || !(0.0..=4.0).contains(&request.gain)
            || !request.pitch.is_finite()
            || !(0.25..=4.0).contains(&request.pitch)
            || !request.range.is_finite()
            || request.range <= 0.0
            || request
                .position
                .is_some_and(|p| p.iter().any(|v| !v.is_finite()))
        {
            return Err(AudioError::Invalid);
        }
        // A persistent emitter owns one loop. Updating its transform/gain never
        // restarts sample position, avoiding repeated starts for machine hums.
        let existing = if request.looping {
            self.voices[..self.voice_limit].iter().position(|v| {
                v.is_some_and(|v| v.request.looping && v.request.emitter == request.emitter)
            })
        } else {
            None
        };
        let slot = if let Some(slot) = existing {
            slot
        } else if let Some(slot) = self.voices[..self.voice_limit]
            .iter()
            .position(Option::is_none)
        {
            slot
        } else {
            let (slot, voice) = self.voices[..self.voice_limit]
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| {
                    let v = v.unwrap();
                    (v.request.priority, v.generation)
                })
                .unwrap();
            if voice.unwrap().request.priority > request.priority {
                self.work.rejected += 1;
                return Err(AudioError::Capacity);
            }
            self.work.stolen += 1;
            slot
        };
        let [left, right] = self.gains(request);
        let generation;
        if let Some(voice) = self.voices[slot].as_mut().filter(|_| existing.is_some()) {
            generation = voice.generation;
            if voice.request.clip != request.clip {
                voice.cursor = 0.0;
            }
            voice.request = request;
            voice.left = left;
            voice.right = right;
            voice.stopping = 0;
        } else {
            generation = self.next_generation;
            self.next_generation = self
                .next_generation
                .checked_add(1)
                .ok_or(AudioError::Capacity)?;
            self.voices[slot] = Some(Voice {
                request,
                cursor: 0.0,
                generation,
                left,
                right,
                stopping: 0,
            });
        }
        self.work.spatial_updates += 1;
        self.work.peak_voices = self.work.peak_voices.max(self.active_voices());
        Ok(VoiceHandle {
            slot: slot as u8,
            generation,
        })
    }
    pub fn stop(&mut self, handle: VoiceHandle) -> bool {
        if let Some(voice) = self
            .voices
            .get_mut(usize::from(handle.slot))
            .and_then(Option::as_mut)
            .filter(|v| v.generation == handle.generation)
        {
            voice.stopping = 128;
            true
        } else {
            false
        }
    }
    pub fn cursor(&self, handle: VoiceHandle) -> Option<f64> {
        self.voices
            .get(usize::from(handle.slot))
            .copied()
            .flatten()
            .filter(|v| v.generation == handle.generation)
            .map(|v| v.cursor)
    }
    pub fn mix_stereo(&mut self, output: &mut [f32]) {
        output.fill(0.0);
        let frames = output.len() / 2;
        self.work.frames += frames as u64;
        for slot in &mut self.voices[..self.voice_limit] {
            let Some(voice) = slot else {
                continue;
            };
            let pcm = &self.clips[usize::from(voice.request.clip.0)];
            let step = f64::from(pcm.rate) / f64::from(self.rate) * f64::from(voice.request.pitch);
            let mut finished = false;
            for frame in output.chunks_exact_mut(2) {
                if voice.cursor >= pcm.samples.len() as f64 {
                    if voice.request.looping {
                        voice.cursor %= pcm.samples.len() as f64;
                    } else {
                        finished = true;
                        break;
                    }
                }
                let index = voice.cursor as usize;
                let next = if index + 1 < pcm.samples.len() {
                    index + 1
                } else if voice.request.looping {
                    0
                } else {
                    index
                };
                let fraction = (voice.cursor - index as f64) as f32;
                let mut sample =
                    pcm.samples[index] + (pcm.samples[next] - pcm.samples[index]) * fraction;
                if voice.stopping > 0 {
                    sample *= f32::from(voice.stopping) / 128.0;
                    voice.stopping -= 1;
                    if voice.stopping == 0 {
                        finished = true;
                    }
                }
                frame[0] += sample * voice.left;
                frame[1] += sample * voice.right;
                voice.cursor += step;
                self.work.voice_samples += 1;
                if finished {
                    break;
                }
            }
            if finished {
                *slot = None;
            }
        }
        if let Some(echo) = &mut self.echo {
            self.work.echo_products += echo.process(output);
        }
        // Fixed limiter, without hidden gain pumping or per-voice allocations.
        for sample in output {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }
    pub fn take_work(&mut self) -> MixWork {
        std::mem::take(&mut self.work)
    }
}
