//! The call's speakers: one mixer thread owning the output device.
//!
//! Each voice slot's decoded audio (48 kHz mono, from libwebrtc) arrives in
//! its own ring buffer. Every 10 ms the mixer takes a frame from each, applies
//! the person's volume and the spatial position the way the browser's
//! PannerNode would (equal-power pan, inverse distance), mixes to stereo,
//! applies output volume, hands the result to echo cancellation as the far
//! end, and queues it for the device.
//!
//! The mixer runs as fast as the device consumes, so the device's clock sets
//! the pace; slots that run ahead have their excess trimmed.

use crate::capture::SharedApm;
use crate::resample::Resampler;
use crate::{devices, fake};
use chatter_media::SAMPLES_PER_FRAME;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// spatialAudio.ts in the client: metres, inverse model.
const REF_DISTANCE: f32 = 1.5;
const ROLLOFF: f32 = 1.0;
const MAX_DISTANCE: f32 = 15.0;
/// How quickly a gain or position change slides in (GLIDE_SECONDS there).
const GLIDE_FRAMES: f32 = 8.0;
/// Audio queued for the device. Lower is less latency; too low underruns.
const TARGET_BUFFER_MS: usize = 40;
/// A slot holding more than this has drifted ahead of the device; trim it.
const SLOT_MAX_MS: usize = 160;

#[derive(Clone, Copy, Default, Debug)]
pub struct Position {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

struct Slot {
    samples: rtrb::Consumer<f32>,
    gain: f32,
    position: Position,
    /// What was actually applied last frame, per ear, so changes glide.
    applied: (f32, f32),
}

/// Told when people in the call start or stop being audible.
pub type SpeechListener = Box<dyn Fn(bool) + Send + Sync>;

struct Shared {
    slots: Mutex<HashMap<u32, Slot>>,
    on_speech: Option<SpeechListener>,
    listener: Mutex<Position>,
    output: Mutex<(String, f32)>,
    output_changed: AtomicBool,
    monitor: Mutex<Option<rtrb::Consumer<f32>>>,
    stop: AtomicBool,
}

pub struct Playout {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The writing end for one slot's audio.
pub type SlotFeed = rtrb::Producer<f32>;

impl Playout {
    pub fn start(apm: SharedApm, on_speech: Option<SpeechListener>) -> Playout {
        let shared = Arc::new(Shared {
            slots: Mutex::new(HashMap::new()),
            on_speech,
            listener: Mutex::new(Position::default()),
            output: Mutex::new(("default".into(), 1.0)),
            output_changed: AtomicBool::new(true),
            monitor: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("playout".into())
            .spawn(move || run(thread_shared, apm))
            .expect("playout thread");
        Playout {
            shared,
            thread: Some(thread),
        }
    }

    /// A new (or replacement) source for a slot. Starts silent until a gain
    /// arrives, as in the browser graph.
    pub fn attach(&self, slot: u32) -> SlotFeed {
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(48_000);
        let mut slots = self.shared.slots.lock();
        let previous = slots.remove(&slot);
        slots.insert(
            slot,
            Slot {
                samples: consumer,
                gain: previous.as_ref().map_or(0.0, |s| s.gain),
                position: previous
                    .as_ref()
                    .map_or_else(Position::default, |s| s.position),
                applied: (0.0, 0.0),
            },
        );
        producer
    }

    pub fn set_gain(&self, slot: u32, gain: f32) {
        if let Some(s) = self.shared.slots.lock().get_mut(&slot) {
            s.gain = gain.clamp(0.0, 4.0);
        }
    }

    pub fn set_position(&self, slot: u32, position: Position) {
        if let Some(s) = self.shared.slots.lock().get_mut(&slot) {
            s.position = position;
        }
    }

    pub fn set_listener(&self, position: Position) {
        *self.shared.listener.lock() = position;
    }

    pub fn set_output(&self, device_id: &str, volume: f32) {
        let mut output = self.shared.output.lock();
        if output.0 != device_id {
            self.shared.output_changed.store(true, Ordering::Release);
        }
        *output = (device_id.to_string(), volume.clamp(0.0, 2.0));
    }

    pub fn clear_slots(&self) {
        self.shared.slots.lock().clear();
    }

    /// Mono 48 kHz audio to play as-is (the mic test's "hear yourself").
    pub fn set_monitor(&self, monitor: Option<rtrb::Consumer<f32>>) {
        *self.shared.monitor.lock() = monitor;
    }
}

impl Drop for Playout {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Left and right gain for a source, as an equal-power PannerNode with the
/// inverse distance model computes them for a listener facing -z.
pub fn spatial_gains(source: Position, listener: Position) -> (f32, f32) {
    let (dx, dy, dz) = (
        source.x - listener.x,
        source.y - listener.y,
        source.z - listener.z,
    );
    let distance = (dx * dx + dy * dy + dz * dz).sqrt();
    let d = distance.clamp(REF_DISTANCE, MAX_DISTANCE);
    let attenuation = REF_DISTANCE / (REF_DISTANCE + ROLLOFF * (d - REF_DISTANCE));
    if distance < 1e-4 {
        // On top of the listener: centred, which equal-power puts at -3 dB a side.
        let centre = std::f32::consts::FRAC_1_SQRT_2;
        return (centre * attenuation, centre * attenuation);
    }
    // Azimuth from straight ahead (-z), positive to the right, folded to the
    // front half the way the Web Audio spec does.
    let mut azimuth = dx.atan2(-dz).to_degrees();
    if azimuth > 90.0 {
        azimuth = 180.0 - azimuth;
    } else if azimuth < -90.0 {
        azimuth = -180.0 - azimuth;
    }
    let x = (azimuth + 90.0) / 180.0;
    let angle = x * std::f32::consts::FRAC_PI_2;
    (angle.cos() * attenuation, angle.sin() * attenuation)
}

struct Output {
    _stream: Option<cpal::Stream>,
    _fake: Option<fake::FakeDevice>,
    queue: rtrb::Producer<f32>,
    rate: u32,
    lost: Arc<AtomicBool>,
    resample: Option<(Resampler, Resampler)>,
}

fn open_output(device_id: &str) -> anyhow::Result<Output> {
    if let Some(path) = fake::output() {
        let (queue, consumer) = rtrb::RingBuffer::<f32>::new(96_000);
        let device = fake::start_output(&path, consumer);
        log::info!("speakers open: fake output {path}");
        return Ok(Output {
            _stream: None,
            _fake: Some(device),
            queue,
            rate: 48_000,
            lost: Arc::new(AtomicBool::new(false)),
            resample: None,
        });
    }
    let device = devices::output(device_id).ok_or_else(|| anyhow::anyhow!("no speakers"))?;
    let supported = device.default_output_config()?;
    // 20 ms per callback; about TARGET_BUFFER_MS queued in the server too.
    let config = devices::stream_config(&supported, 20);
    let channels = config.channels as usize;
    // Stereo frames, interleaved; a second of room.
    let (queue, consumer) = rtrb::RingBuffer::<f32>::new(config.sample_rate as usize * 2);
    let lost = Arc::new(AtomicBool::new(false));
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, config, channels, consumer, lost.clone())?,
        cpal::SampleFormat::I16 => build::<i16>(&device, config, channels, consumer, lost.clone())?,
        cpal::SampleFormat::I32 => build::<i32>(&device, config, channels, consumer, lost.clone())?,
        cpal::SampleFormat::U16 => build::<u16>(&device, config, channels, consumer, lost.clone())?,
        other => anyhow::bail!("unsupported speaker sample format {other:?}"),
    };
    stream.play()?;
    log::info!(
        "speakers open: {} at {} Hz, {} ch, {}",
        device,
        config.sample_rate,
        channels,
        devices::describe_buffer(&stream)
    );
    let resample = (config.sample_rate != 48_000).then(|| {
        (
            Resampler::new(48_000, config.sample_rate),
            Resampler::new(48_000, config.sample_rate),
        )
    });
    Ok(Output {
        _stream: Some(stream),
        _fake: None,
        queue,
        rate: config.sample_rate,
        lost,
        resample,
    })
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut queue: rtrb::Consumer<f32>,
    lost: Arc<AtomicBool>,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let stream = device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _| {
            for frame in data.chunks_mut(channels) {
                // Underrun plays silence rather than stale audio, and only
                // whole pairs are taken so left and right can never swap.
                let (l, r) = if queue.slots() >= 2 {
                    (queue.pop().unwrap_or(0.0), queue.pop().unwrap_or(0.0))
                } else {
                    (0.0, 0.0)
                };
                match frame.len() {
                    1 => frame[0] = T::from_sample((l + r) * 0.5),
                    _ => {
                        frame[0] = T::from_sample(l);
                        frame[1] = T::from_sample(r);
                        for extra in &mut frame[2..] {
                            *extra = T::from_sample(0.0f32);
                        }
                    }
                }
            }
        },
        move |err| match err.kind() {
            cpal::ErrorKind::Xrun => {}
            kind => {
                log::warn!("speaker stream error {kind:?}: {err}");
                lost.store(true, Ordering::Release);
            }
        },
        None,
    )?;
    Ok(stream)
}

fn run(shared: Arc<Shared>, apm: SharedApm) {
    let mut output: Option<Output> = None;
    let mut retry_at = std::time::Instant::now();
    let mut left = vec![0f32; SAMPLES_PER_FRAME];
    let mut right = vec![0f32; SAMPLES_PER_FRAME];
    let mut slot_frame = vec![0f32; SAMPLES_PER_FRAME];
    let mut reverse = vec![0i16; SAMPLES_PER_FRAME * 2];
    let mut res_l = Vec::new();
    let mut res_r = Vec::new();
    let mut speech = SpeechDetector::default();

    while !shared.stop.load(Ordering::Acquire) {
        if shared.output_changed.swap(false, Ordering::AcqRel) {
            output = None;
        }
        if output
            .as_ref()
            .is_some_and(|o| o.lost.load(Ordering::Acquire))
        {
            log::info!("speakers lost; reopening");
            output = None;
        }
        if output.is_none() {
            if std::time::Instant::now() < retry_at {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            let device_id = shared.output.lock().0.clone();
            match open_output(&device_id) {
                Ok(o) => output = Some(o),
                Err(e) => {
                    log::warn!("could not open speakers: {e}");
                    retry_at = std::time::Instant::now() + Duration::from_secs(2);
                    continue;
                }
            }
        }
        let out = output.as_mut().expect("opened above");

        // Keep about TARGET_BUFFER_MS queued; the device drains it.
        let target = out.rate as usize * TARGET_BUFFER_MS / 1000 * 2;
        let queued = out.queue.buffer().capacity() - out.queue.slots();
        if queued >= target {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        left.fill(0.0);
        right.fill(0.0);
        let volume = shared.output.lock().1;
        let listener = *shared.listener.lock();
        {
            let mut slots = shared.slots.lock();
            for slot in slots.values_mut() {
                let excess = slot.samples.slots().saturating_sub(48 * SLOT_MAX_MS);
                if excess > 0 {
                    if let Ok(chunk) = slot.samples.read_chunk(excess) {
                        chunk.commit_all();
                    }
                }
                slot_frame.fill(0.0);
                let _ = slot.samples.pop_partial_slice(&mut slot_frame);

                let (pl, pr) = spatial_gains(slot.position, listener);
                let target = (pl * slot.gain, pr * slot.gain);
                let (from_l, from_r) = slot.applied;
                let to = (
                    from_l + (target.0 - from_l) / GLIDE_FRAMES,
                    from_r + (target.1 - from_r) / GLIDE_FRAMES,
                );
                // Ramp across the frame so the step doesn't click.
                let n = SAMPLES_PER_FRAME as f32;
                for (i, s) in slot_frame.iter().enumerate() {
                    let t = i as f32 / n;
                    left[i] += s * (from_l + (to.0 - from_l) * t);
                    right[i] += s * (from_r + (to.1 - from_r) * t);
                }
                slot.applied = to;
            }
        }
        // Someone in the call audible (before output volume, so turning
        // Chatter down doesn't stop other apps being ducked).
        if let Some(changed) = speech.push(&left, &right) {
            if let Some(listener) = &shared.on_speech {
                listener(changed);
            }
        }

        // The mic test's monitor plays straight through, like the browser's
        // <audio> element does; voice slots are panned like its PannerNode,
        // which also puts a centred speaker at -3 dB a side.
        if let Some(monitor) = shared.monitor.lock().as_mut() {
            slot_frame.fill(0.0);
            let _ = monitor.pop_partial_slice(&mut slot_frame);
            for i in 0..SAMPLES_PER_FRAME {
                left[i] += slot_frame[i];
                right[i] += slot_frame[i];
            }
        }
        for i in 0..SAMPLES_PER_FRAME {
            left[i] = (left[i] * volume).clamp(-1.0, 1.0);
            right[i] = (right[i] * volume).clamp(-1.0, 1.0);
            reverse[i * 2] = (left[i] * i16::MAX as f32) as i16;
            reverse[i * 2 + 1] = (right[i] * i16::MAX as f32) as i16;
        }
        if let Some(apm) = apm.lock().as_mut() {
            let _ = apm.process_reverse_stream(&mut reverse, 48_000, 2);
        }

        let (l_out, r_out): (&[f32], &[f32]) = match out.resample.as_mut() {
            Some((rl, rr)) => {
                res_l.clear();
                res_r.clear();
                rl.process(&left, &mut res_l);
                rr.process(&right, &mut res_r);
                let n = res_l.len().min(res_r.len());
                (&res_l[..n], &res_r[..n])
            }
            None => (&left, &right),
        };
        for (l, r) in l_out.iter().zip(r_out) {
            // Whole pairs only, for the same reason.
            if out.queue.slots() < 2 {
                break;
            }
            let _ = out.queue.push(*l);
            let _ = out.queue.push(*r);
        }
    }
}

/// Voices in the mix, with a hold so ducking doesn't pump between words.
#[derive(Default)]
struct SpeechDetector {
    talking: bool,
    quiet_frames: u32,
}

impl SpeechDetector {
    /// About -40 dBFS: speech after the slot gains, not line noise.
    const THRESHOLD: f32 = 0.01;
    /// 800 ms of 10 ms frames.
    const RELEASE_FRAMES: u32 = 80;

    /// The new state when it changes.
    fn push(&mut self, left: &[f32], right: &[f32]) -> Option<bool> {
        let energy: f32 = left.iter().chain(right).map(|s| s * s).sum();
        let rms = (energy / (left.len() + right.len()) as f32).sqrt();
        if rms > Self::THRESHOLD {
            self.quiet_frames = 0;
            if !self.talking {
                self.talking = true;
                return Some(true);
            }
        } else if self.talking {
            self.quiet_frames += 1;
            if self.quiet_frames >= Self::RELEASE_FRAMES {
                self.talking = false;
                return Some(false);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, z: f32) -> Position {
        Position { x, y: 0.0, z }
    }

    #[test]
    fn centred_is_equal_power() {
        let (l, r) = spatial_gains(at(0.0, 0.0), at(0.0, 0.0));
        assert!((l - r).abs() < 1e-6);
        assert!((l * l + r * r - 1.0).abs() < 1e-4);
    }

    #[test]
    fn right_side_is_louder_on_the_right() {
        let (l, r) = spatial_gains(at(1.0, 0.0), at(0.0, 0.0));
        assert!(r > 0.95 * (l * l + r * r).sqrt());
        assert!(l < 0.05);
    }

    #[test]
    fn distance_follows_the_inverse_model() {
        // 6 m straight ahead: 1.5 / (1.5 + 4.5) = 0.25, centred.
        let (l, r) = spatial_gains(at(0.0, -6.0), at(0.0, 0.0));
        let total = (l * l + r * r).sqrt();
        assert!((total - 0.25).abs() < 1e-3, "{total}");
        // Inside the reference distance stays at full volume.
        let (l, r) = spatial_gains(at(0.0, -1.0), at(0.0, 0.0));
        assert!(((l * l + r * r).sqrt() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn speech_detector_attacks_at_once_and_holds() {
        let mut d = SpeechDetector::default();
        let loud = vec![0.2f32; 480];
        let quiet = vec![0.0f32; 480];
        assert_eq!(d.push(&loud, &loud), Some(true));
        assert_eq!(d.push(&loud, &loud), None);
        for _ in 0..SpeechDetector::RELEASE_FRAMES - 1 {
            assert_eq!(d.push(&quiet, &quiet), None);
        }
        assert_eq!(d.push(&quiet, &quiet), Some(false));
    }

    #[test]
    fn behind_folds_to_the_front() {
        let front = spatial_gains(at(1.0, -1.0), at(0.0, 0.0));
        let back = spatial_gains(at(1.0, 1.0), at(0.0, 0.0));
        assert!((front.0 - back.0).abs() < 1e-4 && (front.1 - back.1).abs() < 1e-4);
    }
}
