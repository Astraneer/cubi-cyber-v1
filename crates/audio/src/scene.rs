//! Loading-only bank preparation and the same allocation-free game event
//! selection for native/WebAudio. No network or telemetry owner can access it.
use crate::{
    AudioError, ClipId, EmitterId, Listener, Mixer, VoiceHandle, VoiceRequest,
    catalog::Catalog,
    events::{StepBody, Steps},
    resources::LocalResources,
};
use std::collections::BTreeMap;
const BANK_BYTES: usize = 256 * 1024 * 1024;
const LOCAL_VARIANTS: usize = 3;
pub struct PreparedSounds {
    mixer: Mixer,
    events: SoundEvents,
}
pub struct SoundEvents {
    catalog: Catalog,
    local: BTreeMap<&'static str, [ClipId; LOCAL_VARIANTS]>,
    steps: [Option<StepEmitter>; 64],
}
#[derive(Clone, Copy)]
struct StepEmitter {
    id: EmitterId,
    steps: Steps,
    last_tick: Option<u64>,
}
impl PreparedSounds {
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn empty() -> Result<Self, AudioError> {
        Ok(Self {
            mixer: Mixer::new(48000, 64, BANK_BYTES)?,
            events: SoundEvents {
                catalog: Catalog::new(),
                local: BTreeMap::new(),
                steps: [None; 64],
            },
        })
    }
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn prepare_file(
        &mut self,
        path: &'static str,
        bytes: &[u8],
    ) -> Result<(), AudioError> {
        self.events
            .catalog
            .prepare_file(&mut self.mixer, path, bytes)
    }
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn prepare_local_variant(
        &mut self,
        local: &LocalResources,
        event: &'static str,
        variant: usize,
    ) -> Result<(), AudioError> {
        let clip = self.events.catalog.prepare_with_local(
            &mut self.mixer,
            local,
            event,
            variant as u64,
            local.minecraft_index(),
        )?;
        self.events
            .local
            .entry(event)
            .or_insert([clip; LOCAL_VARIANTS])[variant] = clip;
        Ok(())
    }
    /// Run during loading, or in a resource-preparation task. Failure preserves
    /// the previous active bank; a caller swaps only a complete prepared result.
    pub fn prepare(
        local: Option<&LocalResources>,
        minecraft_index: Option<&str>,
    ) -> Result<Self, AudioError> {
        let mut mixer = Mixer::new(48000, 64, BANK_BYTES)?;
        let mut catalog = Catalog::new();
        catalog.prepare_all(&mut mixer)?;
        let mut overrides = BTreeMap::new();
        if let Some(local) = local {
            for event in Catalog::events() {
                if local
                    .sound_for_event(event, 0, minecraft_index)
                    .map_err(|_| AudioError::Decode)?
                    .is_none()
                {
                    continue;
                }
                let mut clips = [ClipId(0); LOCAL_VARIANTS];
                for (variant, clip) in clips.iter_mut().enumerate() {
                    *clip = catalog.prepare_with_local(
                        &mut mixer,
                        local,
                        event,
                        variant as u64,
                        minecraft_index,
                    )?;
                }
                overrides.insert(event, clips);
            }
        }
        Ok(Self {
            mixer,
            events: SoundEvents {
                catalog,
                local: overrides,
                steps: [None; 64],
            },
        })
    }
    pub fn into_parts(self) -> (Mixer, SoundEvents) {
        (self.mixer, self.events)
    }
}
impl SoundEvents {
    pub fn request(
        &self,
        event: &str,
        emitter: EmitterId,
        position: Option<[f64; 3]>,
        seed: u64,
    ) -> Result<VoiceRequest, AudioError> {
        let mut request = self
            .catalog
            .request_prepared(event, emitter, position, seed)?;
        if let Some(clips) = self.local.get(event) {
            request.clip = clips[(seed % LOCAL_VARIANTS as u64) as usize];
        }
        Ok(request)
    }
    /// Call once after each fixed simulation tick. Duplicate/replayed tick IDs
    /// cannot generate repeated footsteps; rendering frequency has no effect.
    pub fn steps(
        &mut self,
        tick: u64,
        body: StepBody,
        emitter: EmitterId,
        position: [f64; 3],
    ) -> Result<[Option<VoiceRequest>; 2], AudioError> {
        let slot = self
            .steps
            .iter()
            .position(|value| value.is_some_and(|v| v.id == emitter))
            .or_else(|| self.steps.iter().position(Option::is_none))
            .ok_or(AudioError::Capacity)?;
        let state = self.steps[slot].get_or_insert(StepEmitter {
            id: emitter,
            steps: Steps::default(),
            last_tick: None,
        });
        if state.last_tick.is_some_and(|old| tick <= old) {
            return Ok([None; 2]);
        }
        state.last_tick = Some(tick);
        let cues = state.steps.tick(body);
        let mut result = [None; 2];
        for (index, cue) in cues.into_iter().enumerate() {
            if let Some(cue) = cue {
                let mut request = self.request(
                    cue.event,
                    EmitterId(emitter.0.wrapping_add(index as u64)),
                    Some(position),
                    tick,
                )?;
                request.gain = cue.gain;
                result[index] = Some(request);
            }
        }
        Ok(result)
    }
    pub fn forget_emitter(&mut self, id: EmitterId) {
        if let Some(slot) = self
            .steps
            .iter()
            .position(|value| value.is_some_and(|v| v.id == id))
        {
            self.steps[slot] = None;
        }
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub struct NativeScene {
    output: crate::native::NativeOutput,
    events: SoundEvents,
}
#[cfg(not(target_arch = "wasm32"))]
impl NativeScene {
    pub fn open(prepared: PreparedSounds) -> Result<Self, AudioError> {
        let (mixer, events) = prepared.into_parts();
        Ok(Self {
            output: crate::native::NativeOutput::from_prepared(mixer)?,
            events,
        })
    }
    pub fn output(&self) -> &crate::native::NativeOutput {
        &self.output
    }
    pub fn forget_emitter(&mut self, id: EmitterId) -> Result<usize, AudioError> {
        self.events.forget_emitter(id);
        self.output.with_mixer(|mixer| mixer.stop_emitter(id))
    }
    pub fn event(
        &self,
        event: &str,
        emitter: EmitterId,
        position: Option<[f64; 3]>,
        seed: u64,
    ) -> Result<VoiceHandle, AudioError> {
        let request = self.events.request(event, emitter, position, seed)?;
        self.output.with_mixer(|mixer| mixer.play(request))?
    }
    pub fn steps(
        &mut self,
        tick: u64,
        body: StepBody,
        emitter: EmitterId,
        position: [f64; 3],
    ) -> Result<(), AudioError> {
        let requests = self.events.steps(tick, body, emitter, position)?;
        self.output.with_mixer(|mixer| {
            for request in requests.into_iter().flatten() {
                mixer.play(request)?;
            }
            Ok(())
        })?
    }
    pub fn listener(&self, listener: Listener) -> Result<(), AudioError> {
        self.output.with_mixer(|mixer| mixer.listener(listener))?
    }
}
#[cfg(target_arch = "wasm32")]
pub struct WebScene {
    output: crate::browser::WebOutput,
    events: SoundEvents,
}
#[cfg(target_arch = "wasm32")]
impl WebScene {
    /// AudioContext creation must run in the user's gesture. PCM decoding has
    /// already finished in the loading phase; only AudioBuffer copies remain.
    pub fn open(prepared: PreparedSounds) -> Result<Self, wasm_bindgen::JsValue> {
        let (mixer, events) = prepared.into_parts();
        let mut output = crate::browser::WebOutput::open(64)?;
        for id in 0..crate::mixer::MAX_CLIPS {
            let id = ClipId(id as u16);
            let Some(pcm) = mixer.clip(id) else {
                break;
            };
            output.prepare_clip(id, pcm)?;
        }
        Ok(Self { output, events })
    }
    pub fn output(&self) -> &crate::browser::WebOutput {
        &self.output
    }
    pub fn forget_emitter(&mut self, id: EmitterId) -> Result<usize, wasm_bindgen::JsValue> {
        self.events.forget_emitter(id);
        self.output.stop_emitter(id)
    }
    pub fn output_mut(&mut self) -> &mut crate::browser::WebOutput {
        &mut self.output
    }
    pub fn replace_bank(&mut self, prepared: PreparedSounds) -> Result<(), wasm_bindgen::JsValue> {
        let (mixer, events) = prepared.into_parts();
        self.output.replace_bank(&mixer)?;
        self.events = events;
        Ok(())
    }
    pub fn listener(&mut self, listener: Listener) -> Result<(), wasm_bindgen::JsValue> {
        self.output.listener(listener)
    }
    pub async fn resume(&self) -> Result<(), wasm_bindgen::JsValue> {
        self.output.resume().await
    }
    pub fn event(
        &mut self,
        event: &str,
        emitter: EmitterId,
        position: Option<[f64; 3]>,
        seed: u64,
    ) -> Result<VoiceHandle, wasm_bindgen::JsValue> {
        let request = self
            .events
            .request(event, emitter, position, seed)
            .map_err(|error| wasm_bindgen::JsValue::from_str(&format!("{error:?}")))?;
        self.output.play(request)
    }
    pub fn steps(
        &mut self,
        tick: u64,
        body: StepBody,
        emitter: EmitterId,
        position: [f64; 3],
    ) -> Result<(), wasm_bindgen::JsValue> {
        let requests = self
            .events
            .steps(tick, body, emitter, position)
            .map_err(|error| wasm_bindgen::JsValue::from_str(&format!("{error:?}")))?;
        for request in requests.into_iter().flatten() {
            self.output.play(request)?;
        }
        Ok(())
    }
}
