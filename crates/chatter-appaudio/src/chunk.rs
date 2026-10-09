//! Output shaping shared by every backend: sample-format conversion to stereo `f32`, fixed
//! 10 ms chunking, and the silence pacer that keeps the consumer's clock running.
//!
//! The consumer (the engine) feeds these chunks straight into a WebRTC audio source, which
//! expects exactly 10 ms frames at a steady real-time rate. OS capture APIs give neither: packet
//! sizes vary, and a quiet target often produces *no* packets at all rather than silent ones. So
//! everything is funnelled through a [`Chunker`] (fixed size) and a [`Pacer`] (fills gaps).

use std::time::{Duration, Instant};

/// Output sample rate (Hz).
pub const SAMPLE_RATE: u32 = 48_000;
/// Output channel count (interleaved L, R).
pub const CHANNELS: usize = 2;
/// Frames per delivered chunk (10 ms).
pub const CHUNK_FRAMES: usize = 480;
/// Samples per delivered chunk (`CHUNK_FRAMES * CHANNELS`).
pub const CHUNK_SAMPLES: usize = CHUNK_FRAMES * CHANNELS;
/// Wall-clock length of one chunk.
pub const CHUNK_DURATION: Duration = Duration::from_millis(10);

/// Sample encodings a capture API may hand us.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub enum SampleFormat {
    /// 32-bit IEEE float, little endian.
    F32,
    /// 16-bit signed integer, little endian.
    I16,
}

impl SampleFormat {
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    pub fn bytes(self) -> usize {
        match self {
            SampleFormat::F32 => 4,
            SampleFormat::I16 => 2,
        }
    }
}

/// Appends `data` (interleaved, `channels` per frame, `format` encoded) to `out` as stereo
/// `f32` clamped to -1..1. Mono is duplicated to both sides; with more than two channels only
/// the first two (front left/right in every standard layout) are kept, which is what a
/// stereo screen-share wants and avoids guessing at a downmix matrix. A trailing partial frame
/// is ignored.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub fn append_stereo(data: &[u8], format: SampleFormat, channels: usize, out: &mut Vec<f32>) {
    if channels == 0 {
        return;
    }
    let bps = format.bytes();
    let frame_bytes = bps * channels;
    let frames = data.len() / frame_bytes;
    out.reserve(frames * CHANNELS);
    let sample = |b: &[u8]| -> f32 {
        match format {
            SampleFormat::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            SampleFormat::I16 => f32::from(i16::from_le_bytes([b[0], b[1]])) / 32_768.0,
        }
    };
    for frame in data.chunks_exact(frame_bytes) {
        let l = sample(&frame[..bps]);
        let r = if channels >= 2 {
            sample(&frame[bps..2 * bps])
        } else {
            l
        };
        out.push(clamp(l));
        out.push(clamp(r));
    }
}

/// Clamps to -1..1 and flushes NaN to 0 (a NaN would poison the encoder state downstream).
#[inline]
pub fn clamp(s: f32) -> f32 {
    if s.is_nan() {
        0.0
    } else {
        s.clamp(-1.0, 1.0)
    }
}

/// Regroups an arbitrary-size stereo stream into exact [`CHUNK_SAMPLES`] chunks.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub struct Chunker {
    buf: Vec<f32>,
    /// Total frames ever pushed (including the partial chunk still buffered).
    pushed: u64,
}

impl Default for Chunker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
impl Chunker {
    pub fn new() -> Self {
        Chunker {
            buf: Vec::with_capacity(CHUNK_SAMPLES),
            pushed: 0,
        }
    }

    /// Stream position in frames: everything pushed so far, delivered or still pending.
    pub fn position(&self) -> u64 {
        self.pushed
    }

    /// Frames buffered that do not yet make a full chunk.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pending_frames(&self) -> usize {
        self.buf.len() / CHANNELS
    }

    /// Pushes interleaved stereo samples (an odd trailing sample is dropped) and calls `emit`
    /// once per completed chunk.
    pub fn push(&mut self, mut samples: &[f32], emit: &mut dyn FnMut(&[f32])) {
        samples = &samples[..samples.len() - samples.len() % CHANNELS];
        self.pushed += (samples.len() / CHANNELS) as u64;
        while !samples.is_empty() {
            if self.buf.is_empty() && samples.len() >= CHUNK_SAMPLES {
                // Fast path: emit straight from the input.
                emit(&samples[..CHUNK_SAMPLES]);
                samples = &samples[CHUNK_SAMPLES..];
                continue;
            }
            let take = (CHUNK_SAMPLES - self.buf.len()).min(samples.len());
            self.buf.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.buf.len() == CHUNK_SAMPLES {
                emit(&self.buf);
                self.buf.clear();
            }
        }
    }

    /// Pushes `frames` frames of silence.
    pub fn push_silence(&mut self, frames: u64, emit: &mut dyn FnMut(&[f32])) {
        const ZEROS: [f32; CHUNK_SAMPLES] = [0.0; CHUNK_SAMPLES];
        let mut left = frames;
        while left > 0 {
            let n = left.min(CHUNK_FRAMES as u64);
            self.push(&ZEROS[..n as usize * CHANNELS], emit);
            left -= n;
        }
    }
}

/// Decides when to synthesize silence because the OS has gone quiet.
///
/// Every time real data arrives the pacer is re-anchored: "stream position P corresponds to
/// now". If no further data arrives within the gap threshold, the stream is extended with
/// zeros so its position keeps tracking the wall clock from that anchor. Re-anchoring on
/// every packet means device-clock drift never accumulates into inserted silence while audio
/// is flowing; silence is only ever added across genuine gaps.
///
/// The threshold is 20 ms, or twice the largest packet recently seen if that is longer, so a
/// device that legitimately delivers e.g. 30 ms packets does not get silence wedged between
/// every packet (which would make the output run ahead of real time).
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub struct Pacer {
    anchor: Instant,
    anchor_pos: u64,
    max_packet: Duration,
}

#[cfg_attr(not(any(windows, test)), allow(dead_code))]
impl Pacer {
    /// Minimum gap before silence is inserted.
    pub const MIN_GAP: Duration = Duration::from_millis(20);

    pub fn new(now: Instant, position: u64) -> Self {
        Pacer {
            anchor: now,
            anchor_pos: position,
            max_packet: Duration::ZERO,
        }
    }

    /// Records that a real packet of `frames` frames arrived at `now`, after which the stream
    /// position is `position`.
    pub fn on_data(&mut self, now: Instant, frames: u64, position: u64) {
        let dur = frames_to_duration(frames);
        // Decay slowly so one odd large packet does not raise the threshold forever.
        self.max_packet = dur.max(self.max_packet.mul_f32(0.99));
        self.anchor = now;
        self.anchor_pos = position;
    }

    pub fn gap_threshold(&self) -> Duration {
        Self::MIN_GAP.max(self.max_packet * 2)
    }

    /// Frames of silence to push at `now` given the current stream `position`.
    pub fn silence_due(&self, now: Instant, position: u64) -> u64 {
        let elapsed = now.saturating_duration_since(self.anchor);
        if elapsed <= self.gap_threshold() {
            return 0;
        }
        let expected = self.anchor_pos + duration_to_frames(elapsed);
        expected.saturating_sub(position)
    }
}

/// Wall-clock pacing for backends that pull from buffers (rather than being driven by device
/// packets): how many chunks are due at a given instant.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub struct Ticker {
    start: Instant,
    emitted: u64,
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
impl Ticker {
    /// If the thread falls further behind than this (suspend, debugger, overloaded box) the
    /// backlog is dropped instead of bursting it out.
    pub const MAX_BACKLOG: u64 = 10;

    pub fn new(start: Instant) -> Self {
        Ticker { start, emitted: 0 }
    }

    /// Number of chunks to produce now; the caller must produce exactly that many.
    pub fn due(&mut self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.start);
        let target = (elapsed.as_micros() / CHUNK_DURATION.as_micros()) as u64;
        let mut n = target.saturating_sub(self.emitted);
        if n > Self::MAX_BACKLOG {
            self.emitted = target - 1;
            n = 1;
        }
        self.emitted += n;
        n
    }

    /// When the next chunk becomes due.
    pub fn next_deadline(&self) -> Instant {
        self.start + CHUNK_DURATION * (self.emitted as u32 + 1)
    }
}

pub fn frames_to_duration(frames: u64) -> Duration {
    Duration::from_nanos(frames * 1_000_000_000 / u64::from(SAMPLE_RATE))
}

pub fn duration_to_frames(d: Duration) -> u64 {
    (d.as_nanos() * u128::from(SAMPLE_RATE) / 1_000_000_000) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(chunker: &mut Chunker, input: &[f32], out: &mut Vec<Vec<f32>>) {
        chunker.push(input, &mut |c| out.push(c.to_vec()));
    }

    #[test]
    fn chunker_regroups_arbitrary_sizes() {
        let mut ch = Chunker::new();
        let mut out = Vec::new();
        let total: Vec<f32> = (0..CHUNK_SAMPLES * 3 + 10).map(|i| i as f32).collect();
        // Feed in awkward slices: 7 frames, 1 frame, 1000 frames, rest.
        let mut off = 0;
        for n in [14, 2, 2000] {
            collect(&mut ch, &total[off..off + n], &mut out);
            off += n;
        }
        collect(&mut ch, &total[off..], &mut out);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|c| c.len() == CHUNK_SAMPLES));
        let flat: Vec<f32> = out.concat();
        assert_eq!(&flat[..], &total[..CHUNK_SAMPLES * 3]);
        assert_eq!(ch.pending_frames(), 5);
        assert_eq!(ch.position(), (total.len() / 2) as u64);
    }

    #[test]
    fn chunker_silence() {
        let mut ch = Chunker::new();
        let mut out = Vec::new();
        collect(&mut ch, &[0.5; 200], &mut out); // 100 frames pending
        ch.push_silence(1000, &mut |c| out.push(c.to_vec()));
        assert_eq!(out.len(), 2);
        assert_eq!(ch.pending_frames(), 1100 - 960);
        assert!(out[0][..200].iter().all(|&s| s == 0.5));
        assert!(out[0][200..].iter().all(|&s| s == 0.0));
        assert!(out[1].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn convert_f32_stereo_and_clamp() {
        let mut bytes = Vec::new();
        for s in [0.25f32, -0.5, 2.0, f32::NAN] {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let mut out = Vec::new();
        append_stereo(&bytes, SampleFormat::F32, 2, &mut out);
        assert_eq!(out, vec![0.25, -0.5, 1.0, 0.0]);
    }

    #[test]
    fn convert_i16_mono_duplicates() {
        let mut bytes = Vec::new();
        for s in [16_384i16, -32_768, 0] {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        bytes.push(0xff); // partial trailing frame is ignored
        let mut out = Vec::new();
        append_stereo(&bytes, SampleFormat::I16, 1, &mut out);
        assert_eq!(out, vec![0.5, 0.5, -1.0, -1.0, 0.0, 0.0]);
    }

    #[test]
    fn convert_multichannel_keeps_front_pair() {
        let mut bytes = Vec::new();
        for s in [0.5f32, -0.5, 0.7, 0.7, 0.7, 0.7] {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let mut out = Vec::new();
        append_stereo(&bytes, SampleFormat::F32, 6, &mut out);
        assert_eq!(out, vec![0.5, -0.5]);
    }

    #[test]
    fn pacer_fills_only_after_gap() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0, 0);
        p.on_data(t0, 480, 480);
        assert_eq!(p.silence_due(t0 + Duration::from_millis(15), 480), 0);
        assert_eq!(p.silence_due(t0 + Duration::from_millis(20), 480), 0);
        // 25 ms after the last packet: the stream should be at 480 + 25 ms.
        let due = p.silence_due(t0 + Duration::from_millis(25), 480);
        assert_eq!(due, 1200);
        // Once filled, nothing more is due at the same instant.
        assert_eq!(p.silence_due(t0 + Duration::from_millis(25), 480 + due), 0);
        // And it keeps tracking real time.
        assert_eq!(
            p.silence_due(t0 + Duration::from_millis(35), 480 + due),
            480
        );
    }

    #[test]
    fn pacer_tolerates_large_packets() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0, 0);
        p.on_data(t0, 1440, 1440); // 30 ms packet
        assert_eq!(p.gap_threshold(), Duration::from_millis(60));
        assert_eq!(p.silence_due(t0 + Duration::from_millis(45), 1440), 0);
        assert!(p.silence_due(t0 + Duration::from_millis(70), 1440) > 0);
    }

    #[test]
    fn ticker_paces_and_drops_backlog() {
        let t0 = Instant::now();
        let mut t = Ticker::new(t0);
        assert_eq!(t.due(t0 + Duration::from_millis(5)), 0);
        assert_eq!(t.due(t0 + Duration::from_millis(10)), 1);
        assert_eq!(t.due(t0 + Duration::from_millis(35)), 2);
        assert_eq!(t.next_deadline(), t0 + Duration::from_millis(40));
        // A 1 s stall does not produce a 100-chunk burst.
        assert_eq!(t.due(t0 + Duration::from_millis(1035)), 1);
        assert_eq!(t.due(t0 + Duration::from_millis(1040)), 1);
    }

    #[test]
    fn frame_duration_roundtrip() {
        assert_eq!(frames_to_duration(480), Duration::from_millis(10));
        assert_eq!(duration_to_frames(Duration::from_millis(10)), 480);
    }
}
