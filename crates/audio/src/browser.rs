//! Real WebAudio nodes and local file selection. The only browser I/O here is
//! audio plus File/DOM APIs; no fetch, socket, beacon or server dependency exists.
use crate::{
    AudioError, Category, ClipId, Listener, Pcm, VoiceHandle, VoiceRequest,
    catalog::Catalog,
    echo::Cavity,
    resources::{ImportLimits, LocalResources},
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    AudioBuffer, AudioBufferSourceNode, AudioContext, BaseAudioContext, ConvolverNode, GainNode,
    HtmlInputElement, OfflineAudioContext, StereoPannerNode,
};

struct WebVoice {
    request: VoiceRequest,
    source: AudioBufferSourceNode,
    gain: GainNode,
    pan: StereoPannerNode,
    handle: VoiceHandle,
    started: f64,
    duration: f64,
}
pub struct WebOutput {
    context: AudioContext,
    master: GainNode,
    categories: Vec<GainNode>,
    clips: BTreeMap<ClipId, AudioBuffer>,
    clip_bytes: usize,
    voices: Vec<Option<WebVoice>>,
    listener: Listener,
    next_generation: u64,
    echo: Option<(ConvolverNode, GainNode)>,
}
fn js_error(value: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&value.to_string())
}
fn buffer(context: &BaseAudioContext, pcm: &Pcm) -> Result<AudioBuffer, JsValue> {
    let audio = context.create_buffer(1, pcm.samples().len() as u32, pcm.rate() as f32)?;
    audio.copy_to_channel(pcm.samples(), 0)?;
    Ok(audio)
}
impl WebOutput {
    /// Call from a user gesture. Browser autoplay policy remains enabled.
    pub fn open(voice_limit: usize) -> Result<Self, JsValue> {
        if voice_limit == 0 || voice_limit > crate::mixer::MAX_VOICES {
            return Err(js_error(AudioError::Invalid));
        }
        let context = AudioContext::new()?;
        let master = context.create_gain()?;
        master.connect_with_audio_node(&context.destination())?;
        let mut categories = Vec::with_capacity(crate::mixer::CATEGORIES);
        for _ in 0..crate::mixer::CATEGORIES {
            let gain = context.create_gain()?;
            gain.connect_with_audio_node(&master)?;
            categories.push(gain);
        }
        Ok(Self {
            context,
            master,
            categories,
            clips: BTreeMap::new(),
            clip_bytes: 0,
            voices: (0..voice_limit).map(|_| None).collect(),
            listener: Listener::default(),
            next_generation: 1,
            echo: None,
        })
    }
    pub async fn resume(&self) -> Result<(), JsValue> {
        JsFuture::from(self.context.resume()?).await?;
        Ok(())
    }
    pub fn resume_request(&self) -> Result<js_sys::Promise, JsValue> {
        self.context.resume()
    }
    /// Prepare all buffers before publishing a replacement. Old voices and the
    /// active bank survive any failed preparation; the AudioContext is reused.
    pub fn replace_bank(&mut self, mixer: &crate::Mixer) -> Result<(), JsValue> {
        let mut clips = BTreeMap::new();
        let mut bytes = 0usize;
        for index in 0..crate::mixer::MAX_CLIPS {
            let id = ClipId(index as u16);
            let Some(pcm) = mixer.clip(id) else {
                break;
            };
            bytes = bytes
                .checked_add(pcm.bytes())
                .filter(|n| *n <= 256 * 1024 * 1024)
                .ok_or_else(|| js_error(AudioError::Capacity))?;
            clips.insert(id, buffer(self.context.as_ref(), pcm)?);
        }
        for slot in 0..self.voices.len() {
            if let Some(handle) = self.voices[slot].as_ref().map(|voice| voice.handle) {
                self.stop(handle)?;
            }
        }
        self.clips = clips;
        self.clip_bytes = bytes;
        Ok(())
    }
    pub fn context_state(&self) -> web_sys::AudioContextState {
        self.context.state()
    }
    pub fn prepare_clip(&mut self, id: ClipId, pcm: &Pcm) -> Result<(), JsValue> {
        if !self.clips.contains_key(&id) {
            if self.clips.len() == crate::mixer::MAX_CLIPS
                || pcm.bytes() > (256 * 1024 * 1024usize).saturating_sub(self.clip_bytes)
            {
                return Err(js_error(AudioError::Capacity));
            }
            self.clips.insert(id, buffer(self.context.as_ref(), pcm)?);
            self.clip_bytes += pcm.bytes();
        }
        Ok(())
    }
    pub fn volume(&self, category: Option<Category>, gain: f32) -> Result<(), JsValue> {
        if !gain.is_finite() || !(0.0..=1.0).contains(&gain) {
            return Err(js_error(AudioError::Invalid));
        }
        if let Some(category) = category {
            self.categories[category as usize].gain().set_value(gain);
        } else {
            self.master.gain().set_value(gain);
        }
        Ok(())
    }
    pub fn listener(&mut self, listener: Listener) -> Result<(), JsValue> {
        self.listener = crate::mixer::normalized_listener(listener).map_err(js_error)?;
        for voice in self.voices.iter().flatten() {
            self.position(voice);
        }
        Ok(())
    }
    fn position(&self, voice: &WebVoice) {
        let (pan, attenuation) = if let Some(position) = voice.request.position {
            crate::mixer::spatial_parameters(self.listener, position, voice.request.range)
        } else {
            (0.0, 1.0)
        };
        voice.pan.pan().set_value(pan);
        voice
            .gain
            .gain()
            .set_value(voice.request.gain * attenuation);
    }
    pub fn play(&mut self, request: VoiceRequest) -> Result<VoiceHandle, JsValue> {
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
            return Err(js_error(AudioError::Invalid));
        }
        self.reap();
        let buffer = self
            .clips
            .get(&request.clip)
            .ok_or_else(|| js_error(AudioError::MissingClip))?;
        if request.looping
            && let Some(slot) = self.voices.iter().position(|v| {
                v.as_ref().is_some_and(|v| {
                    v.request.looping
                        && v.request.emitter == request.emitter
                        && v.request.clip == request.clip
                })
            })
        {
            let voice = self.voices[slot].as_mut().unwrap();
            voice.request = request;
            voice.source.playback_rate().set_value(request.pitch);
            let handle = voice.handle;
            self.position(self.voices[slot].as_ref().unwrap());
            return Ok(handle);
        }
        let slot = if let Some(slot) = self.voices.iter().position(Option::is_none) {
            slot
        } else {
            let (slot, voice) = self
                .voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| {
                    let v = v.as_ref().unwrap();
                    (v.request.priority, v.handle.generation)
                })
                .unwrap();
            if voice.as_ref().unwrap().request.priority > request.priority {
                return Err(js_error(AudioError::Capacity));
            }
            slot
        };
        if let Some(old) = self.voices[slot].take() {
            old.source
                .unchecked_ref::<web_sys::AudioScheduledSourceNode>()
                .stop()?;
            old.source.disconnect()?;
            old.gain.disconnect()?;
            old.pan.disconnect()?;
        }
        let source = self.context.create_buffer_source()?;
        source.set_buffer(Some(buffer));
        source.set_loop(request.looping);
        source.playback_rate().set_value(request.pitch);
        let gain = self.context.create_gain()?;
        let pan = self.context.create_stereo_panner()?;
        source.connect_with_audio_node(&gain)?;
        gain.connect_with_audio_node(&pan)?;
        pan.connect_with_audio_node(&self.categories[request.category as usize])?;
        let handle = VoiceHandle {
            slot: slot as u8,
            generation: self.next_generation,
        };
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or_else(|| js_error(AudioError::Capacity))?;
        let voice = WebVoice {
            request,
            source,
            gain,
            pan,
            handle,
            started: self.context.current_time(),
            duration: buffer.duration() / f64::from(request.pitch),
        };
        self.position(&voice);
        voice.source.start()?;
        self.voices[slot] = Some(voice);
        Ok(handle)
    }
    pub fn stop_emitter(&mut self, emitter: crate::EmitterId) -> Result<usize, JsValue> {
        let mut stopped = 0;
        for slot in 0..self.voices.len() {
            let handle = self.voices[slot]
                .as_ref()
                .filter(|voice| voice.request.emitter == emitter)
                .map(|voice| voice.handle);
            if let Some(handle) = handle {
                stopped += usize::from(self.stop(handle)?);
            }
        }
        Ok(stopped)
    }
    pub fn stop(&mut self, handle: VoiceHandle) -> Result<bool, JsValue> {
        let Some(slot) = self.voices.get_mut(usize::from(handle.slot)) else {
            return Ok(false);
        };
        if !slot.as_ref().is_some_and(|v| v.handle == handle) {
            return Ok(false);
        }
        let voice = slot.take().unwrap();
        voice
            .gain
            .gain()
            .set_target_at_time(0.0, self.context.current_time(), 0.003)?;
        voice
            .source
            .unchecked_ref::<web_sys::AudioScheduledSourceNode>()
            .stop_with_when(self.context.current_time() + 0.03)?;
        Ok(true)
    }
    pub fn reap(&mut self) {
        let now = self.context.current_time();
        for slot in &mut self.voices {
            if slot
                .as_ref()
                .is_some_and(|v| !v.request.looping && now - v.started > v.duration + 0.01)
                && let Some(v) = slot.take()
            {
                let _ = v.source.disconnect();
                let _ = v.gain.disconnect();
                let _ = v.pan.disconnect();
            }
        }
    }
    pub fn cavity(&mut self, cavity: Cavity) -> Result<(), JsValue> {
        let impulse = cavity
            .impulse(self.context.sample_rate() as u32)
            .map_err(js_error)?;
        let wet = impulse.iter().any(|v| *v != 0.0);
        let pcm = Pcm::mono(impulse, self.context.sample_rate() as u32).map_err(js_error)?;
        let buffer = buffer(self.context.as_ref(), &pcm)?;
        if wet {
            if self.echo.is_none() {
                let convolver = self.context.create_convolver()?;
                convolver.set_normalize(false);
                let gain = self.context.create_gain()?;
                gain.gain().set_value(0.25);
                convolver.connect_with_audio_node(&gain)?;
                gain.connect_with_audio_node(&self.master)?;
                for category in &self.categories {
                    category.connect_with_audio_node(&convolver)?;
                }
                self.echo = Some((convolver, gain));
            }
            self.echo.as_ref().unwrap().0.set_buffer(Some(&buffer));
        } else if let Some((convolver, gain)) = self.echo.take() {
            for category in &self.categories {
                category.disconnect_with_audio_node(&convolver)?;
            }
            convolver.disconnect()?;
            gain.disconnect()?;
        }
        Ok(())
    }
    pub async fn close(&self) -> Result<(), JsValue> {
        JsFuture::from(self.context.close()?).await?;
        Ok(())
    }
}

thread_local! {static LOCAL:RefCell<LocalResources>=RefCell::new(LocalResources::new(ImportLimits::default()));}
#[derive(Clone, Copy, Debug)]
pub enum ImportEvent {
    Loading,
    Ready(crate::resources::ImportSummary),
    Rejected,
}
thread_local! {static IMPORT_EVENTS:RefCell<VecDeque<ImportEvent>>=const{RefCell::new(VecDeque::new())};}
thread_local! {static IMPORT_EPOCH:Cell<u64>=const{Cell::new(0)};}
fn begin_import() -> Result<u64, JsValue> {
    IMPORT_EPOCH.with(|epoch| {
        let next = epoch
            .get()
            .checked_add(1)
            .ok_or_else(|| js_error("Import generation exhausted"))?;
        epoch.set(next);
        Ok(next)
    })
}
fn current_import(epoch: u64) -> bool {
    IMPORT_EPOCH.with(|current| current.get() == epoch)
}
fn import_event(event: ImportEvent) {
    IMPORT_EVENTS.with(|events| {
        let mut events = events.borrow_mut();
        if events.len() == 4 {
            events.pop_front();
        }
        events.push_back(event);
    });
}
pub fn take_import_event() -> Option<ImportEvent> {
    IMPORT_EVENTS.with(|events| events.borrow_mut().pop_front())
}
pub fn with_local_resources<R>(read: impl FnOnce(&LocalResources) -> R) -> R {
    LOCAL.with(|resources| read(&resources.borrow()))
}
/// Hidden native file-input glue for the GPU-drawn Rust UI. `choose_*` must be
/// called while handling the user's click; status is a bounded Rust event queue.
pub struct BrowserImports {
    pack: web_sys::HtmlElement,
    minecraft: web_sys::HtmlElement,
}
impl BrowserImports {
    pub fn prepare() -> Result<Self, JsValue> {
        let document = web_sys::window()
            .and_then(|w| w.document())
            .ok_or_else(|| js_error("Document unavailable"))?;
        if let (Some(pack), Some(minecraft)) = (
            document.get_element_by_id("cubi-local-pack-action"),
            document.get_element_by_id("cubi-local-minecraft-action"),
        ) {
            return Ok(Self {
                pack: pack.dyn_into()?,
                minecraft: minecraft.dyn_into()?,
            });
        }
        let body = document
            .body()
            .ok_or_else(|| js_error("Body unavailable"))?;
        let pack = document
            .create_element("button")?
            .dyn_into::<web_sys::HtmlElement>()?;
        pack.set_id("cubi-local-pack-action");
        pack.set_hidden(true);
        body.append_child(&pack)?;
        let minecraft = document
            .create_element("button")?
            .dyn_into::<web_sys::HtmlElement>()?;
        minecraft.set_id("cubi-local-minecraft-action");
        minecraft.set_hidden(true);
        body.append_child(&minecraft)?;
        let status = document
            .create_element("span")?
            .dyn_into::<web_sys::HtmlElement>()?;
        status.set_id("cubi-local-import-status");
        status.set_hidden(true);
        body.append_child(&status)?;
        install_pack_picker("cubi-local-pack-action", "cubi-local-import-status")?;
        bind_local_minecraft_picker("cubi-local-minecraft-action", "cubi-local-import-status")?;
        Ok(Self { pack, minecraft })
    }
    pub fn choose_pack(&self) {
        self.pack.click();
    }
    pub fn choose_minecraft(&self) {
        self.minecraft.click();
    }
}
thread_local! {static LIVE:RefCell<Option<WebOutput>>=const{RefCell::new(None)};}
fn summary_json(summary: crate::resources::ImportSummary) -> String {
    format!(
        "{{\"files\":{},\"bytes\":{},\"sounds\":{},\"textures\":{},\"notices\":{},\"revision\":{},\"ignored\":{}}}",
        summary.files,
        summary.bytes,
        summary.sounds,
        summary.textures,
        summary.notices,
        summary.revision,
        summary.ignored
    )
}
#[wasm_bindgen]
pub fn import_local_pack(bytes: &[u8]) -> Result<String, JsValue> {
    begin_import()?;
    finish_pack_import(bytes)
}
fn finish_pack_import(bytes: &[u8]) -> Result<String, JsValue> {
    let result = LOCAL.with(|store| store.borrow_mut().import_zip(bytes));
    match result {
        Ok(summary) => {
            import_event(ImportEvent::Ready(summary));
            Ok(summary_json(summary))
        }
        Err(error) => {
            import_event(ImportEvent::Rejected);
            Err(js_error(error))
        }
    }
}
#[wasm_bindgen]
pub fn local_resource_summary() -> String {
    LOCAL.with(|store| summary_json(store.borrow().summary()))
}
#[wasm_bindgen]
pub fn default_sound_credits() -> String {
    crate::catalog::CREDITS.to_owned()
}
#[wasm_bindgen]
pub fn bind_local_pack_picker(button_id: &str, status_id: &str) -> Result<(), JsValue> {
    install_pack_picker(button_id, status_id)
}
#[wasm_bindgen]
pub fn bind_local_minecraft_picker(button_id: &str, status_id: &str) -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| js_error("Document unavailable"))?;
    let button = document
        .get_element_by_id(button_id)
        .ok_or_else(|| js_error("Import action missing"))?;
    if button.has_attribute("data-cubi-minecraft-picker") {
        return Ok(());
    }
    let status = document
        .get_element_by_id(status_id)
        .ok_or_else(|| js_error("Import status missing"))?;
    let input = document
        .create_element("input")?
        .dyn_into::<HtmlInputElement>()?;
    input.set_type("file");
    input.set_multiple(true);
    input.set_attribute("webkitdirectory", "")?;
    input.set_hidden(true);
    document
        .body()
        .ok_or_else(|| js_error("Body unavailable"))?
        .append_child(&input)?;
    let selected = input.clone();
    let change = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        let Some(files) = selected.files() else {
            return;
        };
        let status = status.clone();
        let Ok(epoch) = begin_import() else {
            return;
        };
        import_event(ImportEvent::Loading);
        spawn_local(async move {
            let result = read_minecraft_selection(files, status.clone(), epoch).await;
            if !current_import(epoch) {
                return;
            }
            if result.is_err() {
                import_event(ImportEvent::Rejected);
            }
            status.set_text_content(Some(&match result {
                Ok(summary) => format!("Звуки Minecraft загружены локально: {summary}"),
                Err(_) => "Локальные звуки отклонены; прежние ресурсы сохранены".into(),
            }));
        });
    });
    input.add_event_listener_with_callback("change", change.as_ref().unchecked_ref())?;
    change.forget();
    let click = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| input.click());
    button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
    click.forget();
    button.set_attribute("data-cubi-minecraft-picker", "")?;
    Ok(())
}
async fn read_minecraft_selection(
    files: web_sys::FileList,
    status: web_sys::Element,
    epoch: u64,
) -> Result<String, JsValue> {
    use crate::resources::{minecraft_sound_objects, selected_minecraft_asset};
    if files.length() > 250000 {
        return Err(js_error("Directory metadata limit"));
    }
    let relative = |file: &web_sys::File| {
        js_sys::Reflect::get(file.as_ref(), &JsValue::from_str("webkitRelativePath"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| file.name())
    };
    let mut indexes = Vec::new();
    for i in 0..files.length() {
        if !current_import(epoch) {
            return Err(js_error("Superseded local selection"));
        }
        let Some(file) = files.get(i) else {
            continue;
        };
        if let Some(path) = selected_minecraft_asset(&relative(&file)).map_err(js_error)?
            && path.starts_with("indexes/")
        {
            if file.size() > 4.0 * 1024.0 * 1024.0 || indexes.len() >= 32 {
                return Err(js_error("Index limit"));
            }
            indexes.push((file.last_modified(), path, file));
        }
    }
    indexes.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let Some((_, index_path, index_file)) = indexes.into_iter().next() else {
        return Err(js_error("No local assets index"));
    };
    let index = js_sys::Uint8Array::new(&JsFuture::from(index_file.array_buffer()).await?).to_vec();
    let needed: std::collections::BTreeSet<_> =
        minecraft_sound_objects(&index, ImportLimits::default().entries - 1)
            .map_err(js_error)?
            .into_iter()
            .collect();
    let mut selected = vec![(index_path, index)];
    let mut total = selected[0].1.len();
    for i in 0..files.length() {
        let Some(file) = files.get(i) else {
            continue;
        };
        let Some(path) = selected_minecraft_asset(&relative(&file)).map_err(js_error)? else {
            continue;
        };
        if !needed.contains(&path) {
            continue;
        }
        if file.size() > ImportLimits::default().entry_bytes as f64
            || file.size() > ImportLimits::default().total_bytes.saturating_sub(total) as f64
        {
            return Err(js_error("Sound byte limit"));
        }
        let data = js_sys::Uint8Array::new(&JsFuture::from(file.array_buffer()).await?).to_vec();
        if !current_import(epoch) {
            return Err(js_error("Superseded local selection"));
        }
        total += data.len();
        selected.push((path, data));
        status.set_text_content(Some(&format!(
            "Загрузка локальных звуков: {}",
            selected.len() - 1
        )));
    }
    LOCAL.with(|store| {
        if !current_import(epoch) {
            return Err(js_error("Superseded local selection"));
        }
        let summary = store
            .borrow_mut()
            .import_directory(selected)
            .map_err(js_error)?;
        import_event(ImportEvent::Ready(summary));
        Ok(summary_json(summary))
    })
}

#[wasm_bindgen]
pub async fn start_audio_benchmark(cave: bool, voices: u32) -> Result<String, JsValue> {
    if voices == 0 || voices > 64 {
        return Err(js_error("Voice budget exceeded"));
    }
    let old = LIVE.with(|live| live.borrow_mut().take());
    if let Some(old) = old {
        old.close().await?;
    }
    let mut output = WebOutput::open(64)?;
    let samples = (0..4800)
        .map(|frame| 0.01 * libm::sinf(frame as f32 * std::f32::consts::TAU * 220.0 / 48000.0))
        .collect();
    let pcm = Pcm::mono(samples, 48000).map_err(js_error)?;
    output.prepare_clip(ClipId(0), &pcm)?;
    output.cavity(Cavity {
        wall_distances: if cave { [Some(5.0); 6] } else { [None; 6] },
        absorption: 0.3,
    })?;
    for id in 0..voices {
        let mut request = VoiceRequest::local(
            ClipId(0),
            crate::EmitterId(u64::from(id)),
            Category::Machines,
        );
        request.looping = true;
        request.position = Some([f64::from(id % 7) - 3.0, 0.0, 5.0]);
        output.play(request)?;
    }
    output.resume().await?;
    let running = output.context_state() == web_sys::AudioContextState::Running;
    let rate = output.context.sample_rate();
    LIVE.with(|live| *live.borrow_mut() = Some(output));
    Ok(format!(
        "{{\"running\":{running},\"voices\":{voices},\"sampleRate\":{rate},\"cave\":{cave}}}"
    ))
}
#[wasm_bindgen]
pub fn set_audio_cave(cave: bool) -> Result<(), JsValue> {
    LIVE.with(|live| {
        let mut live = live.borrow_mut();
        let output = live
            .as_mut()
            .ok_or_else(|| js_error("Audio is not running"))?;
        output.cavity(Cavity {
            wall_distances: if cave { [Some(5.0); 6] } else { [None; 6] },
            absorption: 0.3,
        })
    })
}
#[wasm_bindgen]
pub async fn stop_audio_benchmark() -> Result<(), JsValue> {
    let output = LIVE.with(|live| live.borrow_mut().take());
    if let Some(output) = output {
        output.close().await?;
    }
    Ok(())
}

/// Bind the actual OS file picker to an existing Rust UI action. Only selected
/// bytes enter the local Rust store, and status text contains aggregate counts.
pub fn install_pack_picker(button_id: &str, status_id: &str) -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| js_error("Document unavailable"))?;
    let button = document
        .get_element_by_id(button_id)
        .ok_or_else(|| js_error("Import action missing"))?;
    if button.has_attribute("data-cubi-pack-picker") {
        return Ok(());
    }
    let status = document
        .get_element_by_id(status_id)
        .ok_or_else(|| js_error("Import status missing"))?;
    let input = document
        .create_element("input")?
        .dyn_into::<HtmlInputElement>()?;
    input.set_type("file");
    input.set_accept(".zip,application/zip");
    input.set_hidden(true);
    document
        .body()
        .ok_or_else(|| js_error("Body unavailable"))?
        .append_child(&input)?;
    let selected = input.clone();
    let change = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        let Some(file) = selected.files().and_then(|files| files.get(0)) else {
            return;
        };
        let status = status.clone();
        let Ok(epoch) = begin_import() else {
            return;
        };
        if file.size() > ImportLimits::default().archive_bytes as f64 {
            import_event(ImportEvent::Rejected);
            status.set_text_content(Some("Файл превышает локальный лимит"));
            return;
        }
        import_event(ImportEvent::Loading);
        spawn_local(async move {
            let result = async {
                let bytes = JsFuture::from(file.array_buffer()).await?;
                if !current_import(epoch) {
                    return Err(js_error("Superseded local selection"));
                }
                let bytes = js_sys::Uint8Array::new(&bytes).to_vec();
                finish_pack_import(&bytes)
            }
            .await;
            if !current_import(epoch) {
                return;
            }
            if result.is_err() {
                // `finish_pack_import` already emitted rejection; read failures
                // also need a bounded status event for the Rust UI adapter.
                if !matches!(
                    IMPORT_EVENTS.with(|events| events.borrow().back().copied()),
                    Some(ImportEvent::Rejected)
                ) {
                    import_event(ImportEvent::Rejected);
                }
            }
            status.set_text_content(Some(&match result {
                Ok(summary) => format!("Набор загружен локально: {summary}"),
                Err(_) => "Набор отклонён; прежние ресурсы сохранены".into(),
            }));
        });
    });
    input.add_event_listener_with_callback("change", change.as_ref().unchecked_ref())?;
    change.forget();
    let click = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| input.click());
    button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
    click.forget();
    button.set_attribute("data-cubi-pack-picker", "")?;
    Ok(())
}

async fn impulse_render(cave: bool) -> Result<(Vec<f32>, f64), JsValue> {
    let context = OfflineAudioContext::new_with_number_of_channels_and_length_and_sample_rate(
        1, 48000, 48000.0,
    )?;
    let source = context.create_buffer_source()?;
    let input = context.create_buffer(1, 8, 48000.0)?;
    input.copy_to_channel(&[0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 0)?;
    source.set_buffer(Some(&input));
    source.connect_with_audio_node(&context.destination())?;
    let convolution = context.create_convolver()?;
    convolution.set_normalize(false);
    let impulse = Cavity {
        wall_distances: if cave { [Some(5.0); 6] } else { [None; 6] },
        absorption: 0.3,
    }
    .impulse(48000)
    .map_err(js_error)?;
    let ir = context.create_buffer(1, impulse.len() as u32, 48000.0)?;
    ir.copy_to_channel(&impulse, 0)?;
    convolution.set_buffer(Some(&ir));
    source.connect_with_audio_node(&convolution)?;
    convolution.connect_with_audio_node(&context.destination())?;
    source.start()?;
    let performance = web_sys::window()
        .and_then(|w| w.performance())
        .ok_or_else(|| js_error("Performance unavailable"))?;
    let begin = performance.now();
    let rendered = JsFuture::from(context.start_rendering()?)
        .await?
        .dyn_into::<AudioBuffer>()?;
    let elapsed = performance.now() - begin;
    Ok((rendered.get_channel_data(0)?, elapsed))
}
/// Independent actual WebAudio readback. Reports wall processing separately from
/// the unmeasured H8 CPU-per-quantum target; offline speed is not that target.
#[wasm_bindgen]
pub async fn audio_offline_oracle() -> Result<String, JsValue> {
    let context = OfflineAudioContext::new_with_number_of_channels_and_length_and_sample_rate(
        2, 128, 48000.0,
    )?;
    let source = context.create_buffer_source()?;
    let input = context.create_buffer(1, 8, 48000.0)?;
    input.copy_to_channel(&[0.5; 8], 0)?;
    source.set_buffer(Some(&input));
    let gain = context.create_gain()?;
    gain.gain().set_value(0.5);
    let pan = context.create_stereo_panner()?;
    pan.pan().set_value(1.0);
    source.connect_with_audio_node(&gain)?;
    gain.connect_with_audio_node(&pan)?;
    pan.connect_with_audio_node(&context.destination())?;
    source.start()?;
    let output = JsFuture::from(context.start_rendering()?)
        .await?
        .dyn_into::<AudioBuffer>()?;
    let left = output.get_channel_data(0)?;
    let right = output.get_channel_data(1)?;
    let pan_error = left[..8]
        .iter()
        .map(|v| v.abs())
        .fold(0.0f32, f32::max)
        .max(
            right[..8]
                .iter()
                .map(|v| (v - 0.25).abs())
                .fold(0.0f32, f32::max),
        );
    if pan_error > 0.00001 {
        return Err(js_error("WebAudio pan/gain readback mismatch"));
    }
    let (cave, cave_ms) = impulse_render(true).await?;
    let (plain, plain_ms) = impulse_render(false).await?;
    let tail = cave[128..].iter().map(|v| v * v).sum::<f32>();
    let plain_tail = plain[128..].iter().map(|v| v * v).sum::<f32>();
    let first = cave
        .iter()
        .enumerate()
        .skip(128)
        .find_map(|(i, v)| (v.abs() > 0.00001).then_some(i))
        .ok_or_else(|| js_error("Cave echo missing"))?;
    if tail < 0.0001 || plain_tail > 0.0000001 {
        return Err(js_error("Cave/plain audio not distinguishable"));
    }
    let clip = Pcm::decode_ogg(
        Catalog::compressed("ui/click_1.ogg").ok_or_else(|| js_error("Default sound missing"))?,
    )
    .map_err(js_error)?;
    Ok(format!(
        "{{\"panMaxError\":{pan_error},\"caveTailEnergy\":{tail},\"plainTailEnergy\":{plain_tail},\"firstEchoFrame\":{first},\"offlineCaveMs\":{cave_ms},\"offlinePlainMs\":{plain_ms},\"defaultDecodedFrames\":{},\"h8RealtimeCpuMeasured\":false}}",
        clip.samples().len()
    ))
}
