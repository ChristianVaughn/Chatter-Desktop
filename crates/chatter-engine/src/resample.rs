//! Mono sample-rate conversion in a streaming shape: feed any number of
//! samples, collect whatever comes out.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler as _};

pub struct Resampler {
    inner: Fft<f32>,
    input: Vec<f32>,
    output: Vec<f32>,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        // 10 ms of input per chunk keeps the added latency small.
        let chunk = (from as usize / 100).max(1);
        let inner = Fft::<f32>::new(from as usize, to as usize, chunk, 1, FixedSync::Input)
            .expect("resampler parameters");
        let output = vec![0.0; inner.output_frames_max()];
        Self {
            inner,
            input: Vec::with_capacity(chunk * 4),
            output,
        }
    }

    /// Append the resampled form of `samples` to `out`.
    pub fn process(&mut self, samples: &[f32], out: &mut Vec<f32>) {
        self.input.extend_from_slice(samples);
        loop {
            let need = self.inner.input_frames_next();
            if self.input.len() < need {
                break;
            }
            let frames_out = self.output.len();
            let produced = {
                let Ok(input) = InterleavedSlice::new(&self.input[..need], 1, need) else {
                    break;
                };
                let Ok(mut output) = InterleavedSlice::new_mut(&mut self.output, 1, frames_out)
                else {
                    break;
                };
                match self.inner.process_into_buffer(&input, &mut output, None) {
                    Ok((_, written)) => written,
                    Err(e) => {
                        log::warn!("resample: {e}");
                        0
                    }
                }
            };
            out.extend_from_slice(&self.output[..produced]);
            self.input.drain(..need);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_rate_ratio() {
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        let input = vec![0.25f32; 44_100];
        for chunk in input.chunks(441) {
            r.process(chunk, &mut out);
        }
        // One second in is about one second out, give or take the filter's delay.
        assert!((out.len() as i64 - 48_000).abs() < 2_000, "{}", out.len());
    }

    #[test]
    fn carries_a_tone_through() {
        let mut r = Resampler::new(96_000, 48_000);
        let mut out = Vec::new();
        let input: Vec<f32> = (0..96_000)
            .map(|i| (i as f32 / 96_000.0 * 440.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        r.process(&input, &mut out);
        let tail = &out[out.len() / 2..];
        let rms = (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt();
        assert!((rms - 0.5 / 2f32.sqrt()).abs() < 0.02, "rms {rms}");
    }
}
