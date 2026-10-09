import { app } from "electron";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { appendFileSync, existsSync, mkdirSync, renameSync, statSync } from "node:fs";
import { join } from "node:path";
import { FRAME_BINARY, FRAME_JSON, type EngineEvent, type EngineHello, type EngineResponse } from "../shared/engine";

type EventListener = (event: EngineEvent) => void;

export function logsDir(): string {
  return join(app.getPath("userData"), "logs");
}

/** The engine's log, kept for "Open logs folder": rotated at 5 MB, one old copy. */
function logEngine(chunk: Buffer): void {
  try {
    const dir = logsDir();
    mkdirSync(dir, { recursive: true });
    const file = join(dir, "engine.log");
    if (existsSync(file) && statSync(file).size > 5 * 1024 * 1024) renameSync(file, join(dir, "engine.old.log"));
    appendFileSync(file, chunk);
  } catch {
    // Logging must never take the engine down with it.
  }
}
type BinaryListener = (stream: number, data: Buffer) => void;

const exe = process.platform === "win32" ? "chatter-engine.exe" : "chatter-engine";

/** Where the engine binary is: bundled in a packaged app, the workspace's
 *  cargo output in development, or wherever CHATTER_ENGINE_PATH says. */
function enginePath(): string | null {
  const override = process.env["CHATTER_ENGINE_PATH"];
  if (override) return existsSync(override) ? override : null;
  if (app.isPackaged) {
    const bundled = join(process.resourcesPath, "bin", exe);
    return existsSync(bundled) ? bundled : null;
  }
  // In development, whichever cargo build is newest, so a stale release
  // build never shadows the debug build you just made.
  const builds = ["release", "debug"]
    .map((profile) => join(app.getAppPath(), "../../target", profile, exe))
    .filter((p) => existsSync(p))
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  return builds[0] ?? null;
}

/**
 * Runs chatter-engine as a child process and speaks its framed protocol over
 * stdio (see shared/engine.ts). A crash is answered by a restart with backoff;
 * whoever was using it hears `engine.lost` and starts over.
 */
export class EngineHost {
  private child: ChildProcessWithoutNullStreams | null = null;
  private buffer = Buffer.alloc(0);
  private nextId = 1;
  private readonly pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  private readonly listeners = new Set<EventListener>();
  private readonly binaryListeners = new Set<BinaryListener>();
  private restarts: number[] = [];
  private stopping = false;
  private stopped: Promise<void> | null = null;
  /** Between a crash and the restart's spawn. */
  private restarting = false;
  hello: EngineHello | null = null;

  get pid(): number | undefined {
    return this.child?.pid;
  }
  private helloWaiters: ((h: EngineHello | null) => void)[] = [];
  private readonly readyListeners = new Set<(hello: EngineHello) => void>();

  start(): void {
    const path = enginePath();
    if (!path) {
      console.warn("[engine] chatter-engine not found; native voice and push-to-talk are off");
      this.resolveHello(null);
      return;
    }
    // The engine keeps its crash journal (what it has turned down) in the
    // profile, so another profile's engine never undoes this one's.
    const env: NodeJS.ProcessEnv = { ...process.env, CHATTER_ENGINE_STATE_DIR: app.getPath("userData") };
    // GNOME's GlobalShortcuts portal wants the app's .desktop id for an
    // unsandboxed app (electron-builder names it after executableName).
    if (process.platform === "linux") env["CHATTER_HOTKEYS_APP_ID"] ??= "chatter-desktop";
    const child = spawn(path, [], { stdio: ["pipe", "pipe", "pipe"], windowsHide: true, env });
    this.child = child;
    this.buffer = Buffer.alloc(0);
    child.stdout.on("data", (chunk: Buffer) => this.onData(chunk));
    child.stderr.on("data", (chunk: Buffer) => {
      process.stderr.write(`[engine] ${chunk}`);
      logEngine(chunk);
    });
    child.on("exit", (code, signal) => {
      // A process we've already let go of (shut down, or failed to spawn).
      if (this.child === child) this.onExit(code, signal);
    });
    // Writing to an engine that just died raises EPIPE here; without a
    // listener that would be an uncaught error in the main process.
    child.stdin.on("error", (err) => console.warn("[engine] stdin:", err.message));
    child.on("error", (err) => {
      console.warn("[engine] failed to start:", err.message);
      // No "exit" follows a failed spawn; take the restart path ourselves.
      if (this.child === child) this.onExit(null, null);
    });
  }

  /** Resolves once the engine has said hello, or null if it can't start. */
  ready(timeoutMs = 5000): Promise<EngineHello | null> {
    if (this.hello || (!this.child && !this.restarting)) return Promise.resolve(this.hello);
    return new Promise((resolve) => {
      const timer = setTimeout(() => resolve(this.hello), timeoutMs);
      this.helloWaiters.push((h) => {
        clearTimeout(timer);
        resolve(h);
      });
    });
  }

  private resolveHello(hello: EngineHello | null): void {
    this.hello = hello;
    this.helloWaiters.splice(0).forEach((w) => w(hello));
    if (hello) this.readyListeners.forEach((l) => l(hello));
  }

  /** Every time the engine comes up, including after a restart. */
  onReady(listener: (hello: EngineHello) => void): () => void {
    this.readyListeners.add(listener);
    return () => this.readyListeners.delete(listener);
  }

  /**
   * Ask the engine to stop by closing its stdin: it then puts back what it
   * changed (other apps' ducked volume) and exits. Killed if it hasn't
   * within the grace period.
   */
  shutdown(graceMs = 1500): Promise<void> {
    // Quitting can ask more than once (each window closing); every caller
    // waits for the same stop.
    this.stopped ??= this.stop(graceMs);
    return this.stopped;
  }

  private stop(graceMs: number): Promise<void> {
    this.stopping = true;
    const child = this.child;
    this.child = null;
    this.hello = null;
    this.pending.forEach((w) => w.reject(new Error("engine stopped")));
    this.pending.clear();
    if (!child || child.exitCode !== null) return Promise.resolve();
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        child.kill();
        resolve();
      }, graceMs);
      // "close" rather than "exit": its last log lines are in by then.
      child.once("close", () => {
        clearTimeout(timer);
        resolve();
      });
      child.stdin.end();
    });
  }

  async request(op: string, args: Record<string, unknown> = {}): Promise<unknown> {
    // Mid-restart, wait for it rather than fail: a call's retries land here.
    if (!this.hello && !this.stopping) await this.ready(5000);
    if (!this.child || !this.hello) throw new Error("engine not running");
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.write(FRAME_JSON, Buffer.from(JSON.stringify({ ...args, id, op })));
    });
  }

  sendBinary(stream: number, data: Buffer): void {
    const header = Buffer.alloc(4);
    header.writeUInt32LE(stream);
    this.write(FRAME_BINARY, Buffer.concat([header, data]));
  }

  onEvent(listener: EventListener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  onBinary(listener: BinaryListener): () => void {
    this.binaryListeners.add(listener);
    return () => this.binaryListeners.delete(listener);
  }

  private write(kind: number, payload: Buffer): void {
    const head = Buffer.alloc(5);
    head.writeUInt32LE(payload.length + 1);
    head.writeUInt8(kind, 4);
    const stdin = this.child?.stdin;
    if (stdin?.writable) stdin.write(Buffer.concat([head, payload]));
  }

  private onData(chunk: Buffer): void {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    while (this.buffer.length >= 4) {
      const len = this.buffer.readUInt32LE(0);
      if (this.buffer.length < 4 + len) break;
      const kind = this.buffer.readUInt8(4);
      const payload = this.buffer.subarray(5, 4 + len);
      this.buffer = this.buffer.subarray(4 + len);
      if (kind === FRAME_JSON) this.onJson(payload.toString("utf8"));
      else if (kind === FRAME_BINARY && payload.length >= 4) {
        const stream = payload.readUInt32LE(0);
        const data = payload.subarray(4);
        this.binaryListeners.forEach((l) => l(stream, data));
      }
    }
  }

  private onJson(text: string): void {
    let msg: EngineResponse | EngineEvent;
    try {
      msg = JSON.parse(text);
    } catch {
      console.warn("[engine] unparseable frame");
      return;
    }
    if ("re" in msg) {
      const response = msg as EngineResponse;
      const waiter = this.pending.get(response.re);
      this.pending.delete(response.re);
      if (!waiter) return;
      if (response.ok) waiter.resolve(response.value);
      else waiter.reject(new Error(response.error ?? "engine error"));
    } else if (msg.ev === "hello") {
      this.resolveHello(msg as unknown as EngineHello);
    } else {
      this.listeners.forEach((l) => l(msg as EngineEvent));
    }
  }

  private onExit(code: number | null, signal: NodeJS.Signals | null): void {
    this.child = null;
    const wasRunning = !!this.hello;
    this.hello = null;
    this.pending.forEach((w) => w.reject(new Error("engine stopped")));
    this.pending.clear();
    if (this.stopping) return;
    console.warn(`[engine] exited (code ${code}, signal ${signal})`);
    if (wasRunning) this.listeners.forEach((l) => l({ ev: "engine.lost" }));
    // Back off when it keeps dying rather than spinning.
    const now = Date.now();
    this.restarts = this.restarts.filter((t) => now - t < 60_000);
    this.restarts.push(now);
    if (this.restarts.length > 5) {
      console.warn("[engine] crashing repeatedly; leaving it off until the app restarts");
      this.resolveHello(null);
      return;
    }
    this.restarting = true;
    setTimeout(() => {
      this.restarting = false;
      this.start();
    }, 500 * this.restarts.length);
  }
}

export const engine = new EngineHost();
