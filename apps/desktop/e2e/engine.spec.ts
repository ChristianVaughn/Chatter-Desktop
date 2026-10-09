import { expect, test } from "@playwright/test";
import { execFileSync, spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loudestWindow, readStereoWav } from "./audio";

// chatter-engine on its own, over its stdio protocol, with fake devices: no
// server, no window. Skipped when the engine hasn't been built.
const repoRoot = join(__dirname, "../../..");
const enginePath =
  process.env["CHATTER_ENGINE_PATH"] ??
  join(repoRoot, "target/debug", process.platform === "win32" ? "chatter-engine.exe" : "chatter-engine");

type Msg = Record<string, unknown> & { ev?: string; re?: number };

class Engine {
  private child: ChildProcessWithoutNullStreams;
  private buffer = Buffer.alloc(0);
  private nextId = 1;
  private waiters = new Map<number, (m: Msg) => void>();
  readonly events: Msg[] = [];

  constructor(env: Record<string, string>) {
    this.child = spawn(enginePath, [], { env: { ...process.env, ...env } });
    this.child.stdout.on("data", (chunk: Buffer) => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      while (this.buffer.length >= 4) {
        const len = this.buffer.readUInt32LE(0);
        if (this.buffer.length < 4 + len) break;
        const kind = this.buffer[4];
        const payload = this.buffer.subarray(5, 4 + len);
        this.buffer = this.buffer.subarray(4 + len);
        if (kind !== 0x4a) continue;
        const msg = JSON.parse(payload.toString("utf8")) as Msg;
        if (typeof msg.re === "number") this.waiters.get(msg.re)?.(msg);
        else this.events.push(msg);
      }
    });
  }

  request(op: string, args: Record<string, unknown> = {}): Promise<Msg> {
    const id = this.nextId++;
    const payload = Buffer.from(JSON.stringify({ ...args, id, op }));
    const head = Buffer.alloc(5);
    head.writeUInt32LE(payload.length + 1);
    head[4] = 0x4a;
    this.child.stdin.write(Buffer.concat([head, payload]));
    return new Promise((resolve) => this.waiters.set(id, resolve));
  }

  async waitFor(predicate: (m: Msg) => boolean, timeoutMs = 5000): Promise<Msg> {
    const start = Date.now();
    while (Date.now() - start < timeoutMs) {
      const found = this.events.find(predicate);
      if (found) return found;
      await new Promise((r) => setTimeout(r, 50));
    }
    throw new Error("timed out waiting for an engine event");
  }

  stop(): Promise<number | null> {
    return new Promise((resolve) => {
      if (this.child.exitCode !== null) return resolve(this.child.exitCode);
      this.child.once("exit", (code) => resolve(code));
      this.child.stdin.end();
    });
  }
}

const options = {
  deviceId: "default",
  echoCancellation: true,
  noiseSuppression: "browser",
  autoGainControl: false,
  gain: 1,
};

test.describe("chatter-engine", () => {
  test.skip(!existsSync(enginePath), "build chatter-engine first");

  let engine: Engine;
  let speakers: string;

  test.beforeEach(() => {
    speakers = join(mkdtempSync(join(tmpdir(), "chatter-engine-")), "speakers.wav");
    engine = new Engine({ CHATTER_ENGINE_FAKE_INPUT: "tone:440", CHATTER_ENGINE_FAKE_OUTPUT: speakers });
  });
  test.afterEach(() => engine.stop());

  test("says hello with its features", async () => {
    const hello = await engine.waitFor((m) => m.ev === "hello");
    expect(hello.features).toContain("voice");
  });

  test("lists devices with a default of each kind", async () => {
    const res = await engine.request("devices.list");
    const value = res.value as { inputs: { id: string }[]; outputs: { id: string }[] };
    expect(res.ok).toBe(true);
    expect(value.inputs[0].id).toBe("default");
    expect(value.outputs[0].id).toBe("default");
  });

  test("refuses an unknown operation", async () => {
    const res = await engine.request("nope");
    expect(res.ok).toBe(false);
  });

  test("an open mic reports levels and speaking", async () => {
    expect((await engine.request("mic.open", { micId: "m1", options })).ok).toBe(true);
    const speaking = await engine.waitFor((m) => m.ev === "mic.level" && m.speaking === true, 6000);
    expect(speaking.level as number).toBeGreaterThan(0.01);

    // Muted, it never counts as speaking.
    await engine.request("mic.enable", { micId: "m1", on: false });
    const count = engine.events.length;
    await new Promise((r) => setTimeout(r, 1000));
    const after = engine.events.slice(count).filter((m) => m.ev === "mic.level");
    expect(after.length).toBeGreaterThan(0);
    expect(after.every((m) => m.speaking === false)).toBe(true);
  });

  test("the mic test meters, and monitors to the speakers", async () => {
    expect((await engine.request("test.start", { options, outputDeviceId: "default" })).ok).toBe(true);
    await engine.waitFor((m) => m.ev === "test.level" && (m.level as number) > 5, 6000);
    await engine.request("test.monitor", { on: true });
    await new Promise((r) => setTimeout(r, 3000));
    const { left, rate } = readStereoWav(speakers);
    const heard = loudestWindow(left, rate);
    expect(heard.rms).toBeGreaterThan(0.02);
    expect(Math.abs(heard.hz - 440)).toBeLessThan(25);
    await engine.request("test.stop");
  });

  test("session.reset clears everything", async () => {
    await engine.request("mic.open", { micId: "m1", options });
    await engine.request("peer.create", { peerId: "p1", iceServers: [] });
    expect((await engine.request("session.reset")).ok).toBe(true);
    expect((await engine.request("peer.createOffer", { peerId: "p1" })).ok).toBe(false);
  });

  test("closing its input stops it cleanly", async () => {
    // How the app quits it: the engine then undoes what it changed (ducked
    // volumes) and exits by itself, rather than being killed.
    await engine.waitFor((m) => m.ev === "hello");
    await engine.request("ducking.set", { amount: 0.5 });
    await engine.request("mic.open", { micId: "m1", options });
    const started = Date.now();
    expect(await engine.stop()).toBe(0);
    expect(Date.now() - started).toBeLessThan(1500);
  });

  test("push-to-talk reports the bound key, system-wide", async () => {
    test.skip(process.platform !== "win32", "key injection here is Windows-only");
    const res = await engine.request("ptt.set", { binding: { code: "F13" } });
    expect(res.value).toMatchObject({ code: "F13", label: "F13" });
    execFileSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", join(__dirname, "fixtures/press-f13.ps1")]);
    await engine.waitFor((m) => m.ev === "ptt" && m.down === true);
    await engine.waitFor((m) => m.ev === "ptt" && m.down === false);
  });
});
