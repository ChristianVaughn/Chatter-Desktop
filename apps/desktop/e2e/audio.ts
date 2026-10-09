// Reading back what the engine's fake speakers recorded.

import { readFileSync } from "node:fs";

/** Left channel of a 16-bit stereo WAV, as -1..1 floats. */
export function readStereoWav(path: string): { left: Float32Array; rate: number } {
  const buf = readFileSync(path);
  // Walk the chunks rather than assume a 44-byte header.
  let offset = 12;
  let rate = 48_000;
  let channels = 2;
  while (offset + 8 <= buf.length) {
    const id = buf.toString("ascii", offset, offset + 4);
    const size = buf.readUInt32LE(offset + 4);
    if (id === "fmt ") {
      channels = buf.readUInt16LE(offset + 10);
      rate = buf.readUInt32LE(offset + 12);
    } else if (id === "data") {
      // A file still being written may claim less than it holds.
      const end = Math.min(buf.length, size > 0 ? offset + 8 + size : buf.length);
      const frames = Math.floor((end - offset - 8) / (2 * channels));
      const left = new Float32Array(frames);
      for (let i = 0; i < frames; i++) left[i] = buf.readInt16LE(offset + 8 + i * 2 * channels) / 32768;
      return { left, rate };
    }
    offset += 8 + size + (size % 2);
  }
  return { left: new Float32Array(0), rate };
}

/** Loudest 100 ms window: its RMS and dominant frequency by zero crossings. */
export function loudestWindow(samples: Float32Array, rate: number): { rms: number; hz: number } {
  const win = Math.floor(rate / 10);
  let best = { rms: 0, hz: 0 };
  for (let start = 0; start + win <= samples.length; start += win) {
    let sum = 0;
    let crossings = 0;
    for (let i = start; i < start + win; i++) {
      sum += samples[i] * samples[i];
      if (i > start && samples[i - 1] < 0 !== samples[i] < 0) crossings++;
    }
    const rms = Math.sqrt(sum / win);
    if (rms > best.rms) best = { rms, hz: crossings / 2 / 0.1 };
  }
  return best;
}
