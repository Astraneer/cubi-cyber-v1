//! Native WASAPI/ALSA output through cpal. A missing endpoint is an explicit
//! error; offline mixer tests are not described as device playback evidence.
use crate::{AudioError, Mixer};
use cpal::{
    FromSample, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
pub struct NativeOutput {
    _stream: cpal::Stream,
    mixer: Arc<Mutex<Mixer>>,
    errors: Arc<AtomicU64>,
    missed: Arc<AtomicU64>,
}
impl NativeOutput {
    pub fn open(voices: usize, memory_limit: usize) -> Result<Self, AudioError> {
        Self::from_prepared(Mixer::new(48000, voices, memory_limit)?)
    }
    pub fn from_prepared(mut prepared: Mixer) -> Result<Self, AudioError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or(AudioError::DeviceUnavailable)?;
        let config = device
            .default_output_config()
            .map_err(|_| AudioError::DeviceUnavailable)?;
        let channels = usize::from(config.channels());
        if channels == 0 || channels > 8 {
            return Err(AudioError::UnsupportedDevice);
        }
        prepared.endpoint_rate(config.sample_rate().0)?;
        let mixer = Arc::new(Mutex::new(prepared));
        let errors = Arc::new(AtomicU64::new(0));
        let missed = Arc::new(AtomicU64::new(0));
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => stream::<f32>(
                &device,
                &config.into(),
                channels,
                mixer.clone(),
                errors.clone(),
                missed.clone(),
            )?,
            cpal::SampleFormat::I16 => stream::<i16>(
                &device,
                &config.into(),
                channels,
                mixer.clone(),
                errors.clone(),
                missed.clone(),
            )?,
            cpal::SampleFormat::U16 => stream::<u16>(
                &device,
                &config.into(),
                channels,
                mixer.clone(),
                errors.clone(),
                missed.clone(),
            )?,
            _ => return Err(AudioError::UnsupportedDevice),
        };
        stream.play().map_err(|_| AudioError::DeviceFailure)?;
        Ok(Self {
            _stream: stream,
            mixer,
            errors,
            missed,
        })
    }
    pub fn with_mixer<T>(&self, action: impl FnOnce(&mut Mixer) -> T) -> Result<T, AudioError> {
        let mut mixer = self.mixer.lock().map_err(|_| AudioError::DeviceFailure)?;
        Ok(action(&mut mixer))
    }
    pub fn callback_errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }
    pub fn missed_callbacks(&self) -> u64 {
        self.missed.load(Ordering::Relaxed)
    }
}
fn stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mixer: Arc<Mutex<Mixer>>,
    errors: Arc<AtomicU64>,
    missed: Arc<AtomicU64>,
) -> Result<cpal::Stream, AudioError> {
    device
        .build_output_stream(
            config,
            move |output: &mut [T], _| {
                let Ok(mut mixer) = mixer.try_lock() else {
                    output.fill(T::EQUILIBRIUM);
                    missed.fetch_add(1, Ordering::Relaxed);
                    return;
                };
                let mut scratch = [0.0f32; 2048];
                for block in output.chunks_mut(1024 * channels) {
                    let frames = block.len() / channels;
                    mixer.mix_stereo(&mut scratch[..frames * 2]);
                    for (index, frame) in block.chunks_exact_mut(channels).enumerate() {
                        if channels == 1 {
                            frame[0] =
                                T::from_sample((scratch[index * 2] + scratch[index * 2 + 1]) * 0.5);
                        } else {
                            frame[0] = T::from_sample(scratch[index * 2]);
                            frame[1] = T::from_sample(scratch[index * 2 + 1]);
                            for value in &mut frame[2..] {
                                *value = T::EQUILIBRIUM;
                            }
                        }
                    }
                }
            },
            move |_| {
                errors.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|_| AudioError::DeviceFailure)
}
