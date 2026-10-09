import { expect, test } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ensureLoggedIn, launchAt, quit, SERVER, storedUser } from "./chatter";

// Screen sharing with an app's own audio: a separate process plays a 1 kHz
// tone, the share is started from Chatter's UI, the desktop app's picker is
// told to take that process's audio, and the share's stream must carry the
// tone — on its way out to the server, too.
const ROOM_NAME = process.env["CHATTER_E2E_ROOM_NAME"] ?? "Native voice spike";
const repoRoot = join(__dirname, "../../..");
const bin = (name: string) => join(repoRoot, "target/debug", process.platform === "win32" ? `${name}.exe` : name);

function toneWav(seconds: number, hz: number): string {
  const rate = 48_000;
  const frames = rate * seconds;
  const data = Buffer.alloc(frames * 2);
  for (let i = 0; i < frames; i++) data.writeInt16LE(Math.round(Math.sin((2 * Math.PI * hz * i) / rate) * 0.4 * 32767), i * 2);
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
  const path = join(mkdtempSync(join(tmpdir(), "chatter-tone-")), "tone.wav");
  writeFileSync(path, Buffer.concat([header, data]));
  return path;
}

test("a screen share carries the chosen app's audio", async () => {
  test.skip(process.platform !== "win32", "the tone player here is Windows-only");
  test.skip(!SERVER || !storedUser("browser_user"), "needs a local server and test accounts");
  test.setTimeout(120_000);

  const player = spawn("powershell", ["-NoProfile", "-Command", `(New-Object Media.SoundPlayer '${toneWav(40, 1000)}').PlaySync()`]);
  const { app } = await launchAt(["--use-fake-device-for-media-stream"], "browser_user", { CHATTER_ENGINE_PATH: bin("chatter-engine") });
  try {
    const page = await app.firstWindow();
    // Keep the audio tracks added to streams, and every peer connection.
    await page.addInitScript(() => {
      const w = window as unknown as { __added: MediaStreamTrack[]; __pcs: RTCPeerConnection[] };
      w.__added = [];
      w.__pcs = [];
      const addTrack = MediaStream.prototype.addTrack;
      MediaStream.prototype.addTrack = function (track: MediaStreamTrack) {
        if (track.kind === "audio") w.__added.push(track);
        return addTrack.call(this, track);
      };
      const Native = window.RTCPeerConnection;
      window.RTCPeerConnection = class extends Native {
        constructor(config?: RTCConfiguration) {
          super(config);
          w.__pcs.push(this);
        }
      } as typeof RTCPeerConnection;
    });
    await page.reload();
    await ensureLoggedIn(page, "browser_user");
    const features = await page.evaluate(() => (window as unknown as { chatterDesktop?: { features: string[] } }).chatterDesktop?.features ?? []);
    test.skip(!features.includes("app-audio@1"), "this Windows can't capture app audio (needs build 20348+)");

    await page.evaluate(() => localStorage.setItem("chatter_native_voice", "off"));
    await page.getByText(ROOM_NAME, { exact: true }).first().click();
    await page.getByText("General", { exact: true }).click();
    await page.waitForTimeout(3_000);

    // Start sharing; the desktop app's own picker opens.
    const pickerOpened = app.waitForEvent("window");
    await page.getByTitle("Share screen").first().click();
    const picker = await pickerOpened;
    await picker.waitForSelector(".source");
    await picker.locator("[role=tab][data-kind=screen]").click();
    await picker.locator(".source").first().click();
    const option = `app:${player.pid}`;
    const values = await picker.locator("#audio option").evaluateAll((els) => els.map((e) => (e as HTMLOptionElement).value));
    expect(values, "the tone player is offered").toContain(option);
    await picker.selectOption("#audio", option);
    await picker.screenshot({ path: "test-results/picker.png" });
    await picker.click("#share");

    await page.waitForTimeout(6_000);
    // The share's stream gained an audio track that carries the tone.
    const heard = await page.evaluate(async () => {
      const track = (window as unknown as { __added: MediaStreamTrack[] }).__added.at(-1);
      if (!track) return { rms: 0, hz: 0 };
      const ctx = new AudioContext({ sampleRate: 48_000 });
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 4096;
      ctx.createMediaStreamSource(new MediaStream([track])).connect(analyser);
      await new Promise((r) => setTimeout(r, 500));
      const buf = new Float32Array(analyser.fftSize);
      analyser.getFloatTimeDomainData(buf);
      let sum = 0;
      let crossings = 0;
      for (let i = 0; i < buf.length; i++) {
        sum += buf[i] * buf[i];
        if (i && buf[i - 1] < 0 !== buf[i] < 0) crossings++;
      }
      await ctx.close();
      return { rms: Math.sqrt(sum / buf.length), hz: crossings / 2 / (buf.length / 48_000) };
    });
    expect(heard.rms, "share audio level").toBeGreaterThan(0.05);
    expect(Math.abs(heard.hz - 1000), `share audio pitch ${heard.hz} Hz`).toBeLessThan(60);

    // And it's being sent: the connection carrying video also sends audio.
    const sent = await page.evaluate(async () => {
      let best = 0;
      for (const pc of (window as unknown as { __pcs: RTCPeerConnection[] }).__pcs) {
        let video = 0;
        let audio = 0;
        (await pc.getStats()).forEach((r) => {
          if (r.type === "outbound-rtp" && r.kind === "video") video += r.bytesSent;
          if (r.type === "outbound-rtp" && r.kind === "audio") audio += r.bytesSent;
        });
        if (video > 0) best = Math.max(best, audio);
      }
      return best;
    });
    expect(sent, "audio bytes sent with the share").toBeGreaterThan(10_000);
    await page.getByTitle("Stop sharing").first().click();
  } finally {
    player.kill();
    await quit(app);
  }
});
