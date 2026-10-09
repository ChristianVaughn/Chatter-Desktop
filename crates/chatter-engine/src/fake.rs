//! Stand-in audio devices for tests, chosen by environment variable:
//!
//! - `CHATTER_ENGINE_FAKE_INPUT`: `tone:<hz>` or a WAV path, looped as the
//!   microphone in real time.
//! - `CHATTER_ENGINE_FAKE_OUTPUT`: a WAV path the speakers are recorded to
//!   (48 kHz stereo), in real time.
//!
//! With neither set nothing here runs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn input() -> Option<String> {
    std::env::var("CHATTER_ENGINE_FAKE_INPUT")
        .ok()
        .filter(|s| !s.is_empty())
}

pub fn output() -> Option<String> {
    std::env::var("CHATTER_ENGINE_FAKE_OUTPUT")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Stops the thread behind a fake device when dropped.
pub struct FakeDevice {
    stop: Arc<AtomicBool>,
}

impl Drop for FakeDevice {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn load(spec: &str) -> Vec<f32> {
    if let Some(hz) = spec
        .strip_prefix("tone:")
        .and_then(|h| h.parse::<f32>().ok())
    {
        // A second of tone and a second of silence, like the test client's.
        return (0..96_000)
            .map(|i| {
                if i >= 48_000 {
                    0.0
                } else {
                    (i as f32 / 48_000.0 * hz * std::f32::consts::TAU).sin() * 0.25
                }
            })
            .collect();
    }
    match hound::WavReader::open(spec) {
        Ok(mut reader) => {
            let spec = reader.spec();
            let ch = spec.channels as usize;
            let raw: Vec<f32> = match spec.sample_format {
                hound::SampleFormat::Int => {
                    let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
                    reader
                        .samples::<i32>()
                        .filter_map(Result::ok)
                        .map(|v| v as f32 / scale)
                        .collect()
                }
                hound::SampleFormat::Float => {
                    reader.samples::<f32>().filter_map(Result::ok).collect()
                }
            };
            raw.chunks(ch)
                .map(|c| c.iter().sum::<f32>() / ch as f32)
                .collect()
        }
        Err(e) => {
            log::warn!("fake input {spec}: {e}; using silence");
            vec![0.0; 480]
        }
    }
}

/// A 48 kHz "microphone" pushing mono samples in 10 ms steps.
pub fn start_input(spec: &str, mut producer: rtrb::Producer<f32>) -> FakeDevice {
    let samples = load(spec);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    std::thread::spawn(move || {
        let start = Instant::now();
        let mut pos = 0usize;
        let mut sent = 0u64;
        while !thread_stop.load(Ordering::Acquire) {
            let due = start.elapsed().as_millis() as u64 * 48;
            while sent < due {
                let _ = producer.push(samples[pos]);
                pos = (pos + 1) % samples.len();
                sent += 1;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    FakeDevice { stop }
}

/// 48 kHz stereo "speakers" draining interleaved samples to a WAV file.
pub fn start_output(path: &str, mut consumer: rtrb::Consumer<f32>) -> FakeDevice {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let path = path.to_string();
    std::thread::spawn(move || {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = match hound::WavWriter::create(&path, spec) {
            Ok(w) => w,
            Err(e) => return log::warn!("fake output {path}: {e}"),
        };
        let start = Instant::now();
        let mut taken = 0u64;
        while !thread_stop.load(Ordering::Acquire) {
            let due = start.elapsed().as_millis() as u64 * 48 * 2;
            while taken < due {
                let s = consumer.pop().unwrap_or(0.0);
                let _ = writer.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                taken += 1;
            }
            // Keep the file readable while it grows.
            let _ = writer.flush();
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = writer.finalize();
    });
    FakeDevice { stop }
}
