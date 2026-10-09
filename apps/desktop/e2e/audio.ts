// Reading back what the engine's fake speakers recorded.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

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

/** Write a mono 16-bit 48 kHz WAV of a steady tone, unless it's already there. */
export function ensureToneWav(path: string, hz: number, seconds = 3): string {
  if (existsSync(path)) return path;
  const rate = 48_000;
  const frames = rate * seconds;
  const data = Buffer.alloc(frames * 2);
  for (let i = 0; i < frames; i++) data.writeInt16LE(Math.round(Math.sin((2 * Math.PI * hz * i) / rate) * 0.25 * 32767), i * 2);
  const header = Buffer.alloc(44);
  header.write("RIFF", 0);
  header.writeUInt32LE(36 + data.length, 4);
  header.write("WAVEfmt ", 8);
  header.writeUInt32LE(16, 16);
  header.writeUInt16LE(1, 20);
  header.writeUInt16LE(1, 22);
  header.writeUInt32LE(rate, 24);
  header.writeUInt32LE(rate * 2, 28);
  header.writeUInt16LE(2, 32);
  header.writeUInt16LE(16, 34);
  header.write("data", 36);
  header.writeUInt32LE(data.length, 40);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, Buffer.concat([header, data]));
  return path;
}
