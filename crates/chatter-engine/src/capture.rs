//! The microphone pipeline, one thread per open mic:
//!
//! device (cpal) → mono → resample to 48 kHz → 10 ms frames →
//! libwebrtc audio processing (echo cancellation, noise suppression, gain
//! control) → RNNoise (enhanced suppression) → input gain → level → sink.
//!
//! The thread owns the device stream, so a device that disappears or a
//! changed choice of device is handled where the stream lives: drop it, open
//! the new one, carry on.

use crate::resample::Resampler;
use crate::{devices, fake};
use chatter_media::libwebrtc::native::apm::AudioProcessingModule;
use chatter_media::SAMPLES_PER_FRAME;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Sample, SizedSample};
use parking_lot::Mutex;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicOptions {
    pub device_id: String,
    pub echo_cancellation: bool,
    /// "none", "browser" (libwebrtc's suppressor) or "rnnoise".
    pub noise_suppression: String,
    pub auto_gain_control: bool,
    pub gain: f32,
}

/// The audio processing module shared by the call's mic and playout: echo
/// cancellation needs to see what is played (`process_reverse_stream`) in the
/// same instance that cleans the mic.
pub type SharedApm = Arc<Mutex<Option<AudioProcessingModule>>>;

pub fn build_apm(options: &MicOptions) -> AudioProcessingModule {
    AudioProcessingModule::new(
        options.echo_cancellation,
        options.auto_gain_control,
        true,
        options.noise_suppression == "browser",
    )
}

/// Where processed 10 ms frames go: 480 mono samples at 48 kHz, -1..1.
pub type FrameSink = Box<dyn FnMut(&[f32]) + Send>;
/// Called about ten times a second with the level (RMS, 0..1) and whether it
/// counts as speech.
pub type LevelSink = Box<dyn FnMut(f32, bool) + Send>;

struct Shared {
    options: Mutex<MicOptions>,
    changed: AtomicBool,
    stop: AtomicBool,
}

pub struct Capture {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    /// `apm` is rebuilt from the options whenever they change; pass a fresh
    /// one for anything that isn't the call (the mic test).
    pub fn start(
        options: MicOptions,
        apm: SharedApm,
        sink: FrameSink,
        levels: LevelSink,
    ) -> Capture {
        let shared = Arc::new(Shared {
            options: Mutex::new(options),
            changed: AtomicBool::new(true),
            stop: AtomicBool::new(false),
        });
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("mic".into())
            .spawn(move || run(thread_shared, apm, sink, levels))
            .expect("mic thread");
        Capture {
            shared,
            thread: Some(thread),
        }
    }

    pub fn update(&self, options: MicOptions) {
        *self.shared.options.lock() = options;
        self.shared.changed.store(true, Ordering::Release);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// An open input stream feeding mono samples into a ring buffer.
struct Input {
    _stream: Option<cpal::Stream>,
    _fake: Option<fake::FakeDevice>,
    samples: rtrb::Consumer<f32>,
    rate: u32,
    lost: Arc<AtomicBool>,
    device_id: String,
}

fn open_input(device_id: &str) -> anyhow::Result<Input> {
    if let Some(spec) = fake::input() {
        let (producer, samples) = rtrb::RingBuffer::<f32>::new(24_000);
        let device = fake::start_input(&spec, producer);
        log::info!("mic open: fake input {spec}");
        return Ok(Input {
            _stream: None,
            _fake: Some(device),
            samples,
            rate: 48_000,
            lost: Arc::new(AtomicBool::new(false)),
            device_id: device_id.to_string(),
        });
    }
    let device = devices::input(device_id).ok_or_else(|| anyhow::anyhow!("no microphone"))?;
    let supported = device.default_input_config()?;
    let config = supported.config();
    let channels = config.channels as usize;
    // Half a second of slack; the DSP thread drains it every few ms.
    let (producer, samples) = rtrb::RingBuffer::<f32>::new(config.sample_rate as usize / 2);
    let lost = Arc::new(AtomicBool::new(false));
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, config, channels, producer, lost.clone())?,
        cpal::SampleFormat::I16 => build::<i16>(&device, config, channels, producer, lost.clone())?,
        cpal::SampleFormat::I32 => build::<i32>(&device, config, channels, producer, lost.clone())?,
        cpal::SampleFormat::U16 => build::<u16>(&device, config, channels, producer, lost.clone())?,
        other => anyhow::bail!("unsupported microphone sample format {other:?}"),
    };
    stream.play()?;
    log::info!(
        "mic open: {} at {} Hz, {} ch",
        device,
        config.sample_rate,
        channels
    );
    Ok(Input {
        _stream: Some(stream),
        _fake: None,
        samples,
        rate: config.sample_rate,
        lost,
        device_id: device_id.to_string(),
    })
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut producer: rtrb::Producer<f32>,
    lost: Arc<AtomicBool>,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample + Send + 'static,
    f32: cpal::FromSample<T>,
{
    let stream = device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            for frame in data.chunks(channels) {
                let mono =
                    frame.iter().map(|s| f32::from_sample(*s)).sum::<f32>() / channels as f32;
                // A full ring means the DSP thread stalled; dropping is the
                // only real-time-safe answer.
                let _ = producer.push(mono);
            }
        },
        move |err| match err.kind() {
            cpal::ErrorKind::Xrun => {}
            kind => {
                log::warn!("mic stream error {kind:?}: {err}");
                lost.store(true, Ordering::Release);
            }
        },
        None,
    )?;
    Ok(stream)
}

fn run(shared: Arc<Shared>, apm: SharedApm, mut sink: FrameSink, mut levels: LevelSink) {
    let mut input: Option<Input> = None;
    let mut resampler: Option<Resampler> = None;
    let mut options = shared.options.lock().clone();
    let mut denoise: Option<Box<nnnoiseless::DenoiseState<'static>>> = None;
    let mut denoise_warm = false;
    let mut pending: Vec<f32> = Vec::with_capacity(SAMPLES_PER_FRAME * 4);
    let mut frame_i16 = vec![0i16; SAMPLES_PER_FRAME];
    let mut frame_f32 = vec![0f32; SAMPLES_PER_FRAME];
    let mut denoised = vec![0f32; SAMPLES_PER_FRAME];
    let mut meter = Meter::new();
    let mut retry_at = Instant::now();
    let mut apm_for: Option<(bool, bool, bool)> = None;

    while !shared.stop.load(Ordering::Acquire) {
        if shared.changed.swap(false, Ordering::AcqRel) {
            let next = shared.options.lock().clone();
            if input
                .as_ref()
                .is_some_and(|i| i.device_id != next.device_id)
            {
                input = None;
            }
            // Rebuilding the processing throws away what the echo canceller
            // and gain control have learned, so only when they change — not
            // for every step of the input-volume slider.
            let processing = (
                next.echo_cancellation,
                next.auto_gain_control,
                next.noise_suppression == "browser",
            );
            if apm_for != Some(processing) || apm.lock().is_none() {
                *apm.lock() = Some(build_apm(&next));
                apm_for = Some(processing);
            }
            if next.noise_suppression == "rnnoise" && denoise.is_none() {
                denoise = Some(nnnoiseless::DenoiseState::new());
                denoise_warm = false;
            } else if next.noise_suppression != "rnnoise" {
                denoise = None;
            }
            options = next;
        }

        if input
            .as_ref()
            .is_some_and(|i| i.lost.load(Ordering::Acquire))
        {
            log::info!("mic lost; reopening");
            input = None;
        }
        if input.is_none() {
            if Instant::now() < retry_at {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            match open_input(&options.device_id) {
                Ok(opened) => {
                    resampler =
                        (opened.rate != 48_000).then(|| Resampler::new(opened.rate, 48_000));
                    pending.clear();
                    input = Some(opened);
                }
                Err(e) => {
                    log::warn!("could not open mic: {e}");
                    retry_at = Instant::now() + Duration::from_secs(2);
                    continue;
                }
            }
        }

        let Some(current) = input.as_mut() else {
            continue;
        };
        let available = current.samples.slots();
        if available == 0 {
            std::thread::sleep(Duration::from_millis(3));
            continue;
        }
        let mut raw = vec![0f32; available];
        if current.samples.pop_entire_slice(&mut raw).is_err() {
            continue;
        }
        match resampler.as_mut() {
            Some(r) => r.process(&raw, &mut pending),
            None => pending.extend_from_slice(&raw),
        }

        while pending.len() >= SAMPLES_PER_FRAME {
            for (dst, src) in frame_i16.iter_mut().zip(&pending[..SAMPLES_PER_FRAME]) {
                *dst = (src.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            }
            pending.drain(..SAMPLES_PER_FRAME);

            if let Some(apm) = apm.lock().as_mut() {
                if let Err(e) = apm.process_stream(&mut frame_i16, 48_000, 1) {
                    log::debug!("apm: {e:?}");
                }
            }

            if let Some(state) = denoise.as_mut() {
                // RNNoise works in i16-scaled floats.
                for (dst, src) in frame_f32.iter_mut().zip(&frame_i16) {
                    *dst = *src as f32;
                }
                state.process_frame(&mut denoised, &frame_f32);
                // Its first frame is a fade-in artefact.
                if !denoise_warm {
                    denoised.fill(0.0);
                    denoise_warm = true;
                }
                for (dst, src) in frame_f32.iter_mut().zip(&denoised) {
                    *dst = (src / i16::MAX as f32 * options.gain).clamp(-1.0, 1.0);
                }
            } else {
                for (dst, src) in frame_f32.iter_mut().zip(&frame_i16) {
                    *dst = (*src as f32 / i16::MAX as f32 * options.gain).clamp(-1.0, 1.0);
                }
            }

            if let Some((level, speaking)) = meter.push(&frame_f32) {
                levels(level, speaking);
            }
            sink(&frame_f32);
        }
    }
}

/// RMS over 100 ms, and a speaking flag with a short hold so it doesn't
/// flicker between words.
struct Meter {
    sum: f32,
    count: usize,
    hold_until: Option<Instant>,
}

impl Meter {
    /// About -46 dBFS: ordinary speech clears it, room noise after
    /// suppression doesn't.
    const SPEAKING_RMS: f32 = 0.005;
    const HOLD: Duration = Duration::from_millis(300);

    fn new() -> Self {
        Self {
            sum: 0.0,
            count: 0,
            hold_until: None,
        }
    }

    fn push(&mut self, frame: &[f32]) -> Option<(f32, bool)> {
        self.sum += frame.iter().map(|s| s * s).sum::<f32>();
        self.count += frame.len();
        if self.count < SAMPLES_PER_FRAME * 10 {
            return None;
        }
        let rms = (self.sum / self.count as f32).sqrt();
        self.sum = 0.0;
        self.count = 0;
        let now = Instant::now();
        if rms > Self::SPEAKING_RMS {
            self.hold_until = Some(now + Self::HOLD);
        }
        let speaking = self.hold_until.is_some_and(|t| now < t);
        Some((rms, speaking))
    }
}
