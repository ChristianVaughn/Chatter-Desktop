// Runs in the page's own JavaScript world, not the preload's. Electron
// serialises `installDesktopBridge` and evaluates it there, so it must be
// entirely self-contained: no imports at runtime, no references to anything
// outside its body. Types are fine — they are erased.
//
// It builds `window.chatterDesktop` (lib/desktop/bridge.ts in the Chatter
// client), including a VoiceMediaBackend (lib/media/types.ts) whose peers,
// microphone and playout live in chatter-engine. Objects the client mutates
// (`pc.onicecandidate = …`) have to be real main-world objects, which is why
// this isn't a contextBridge export.

import type { ChatterDesktopBridge, PttBinding } from "../contract/bridge";
import type {
  AudioDevice,
  LocalMic,
  MicOptions,
  MicTest,
  VoiceIceCandidate,
  VoiceMediaBackend,
  VoicePeer,
  VoiceSender,
  VoiceStatsReport,
  VoiceTrackEvent,
  WorldPosition,
} from "../contract/media";

/** What the preload exposes on `window.__chatterEngine`. */
export interface EngineChannel {
  request(op: string, args?: Record<string, unknown>): Promise<unknown>;
  onEvent(listener: (event: { ev: string; [k: string]: unknown }) => void): void;
  pttGet(): Promise<PttBinding | null>;
  pttCapture(): Promise<PttBinding | null>;
  pttClear(): Promise<void>;
}

export interface BridgeBase {
  bridgeVersion: 1;
  appVersion: string;
  platform: string;
  features: string[];
}

export function installDesktopBridge(base: BridgeBase): void {
  const w = window as unknown as {
    __chatterEngine?: EngineChannel;
    chatterDesktop?: ChatterDesktopBridge;
  };
  const engine = w.__chatterEngine;
  const features = new Set(base.features);
  if (!engine) {
    w.chatterDesktop = Object.freeze({ ...base, features: [] });
    return;
  }

  // ─── Events ────────────────────────────────────────────────────────────
  type Ev = { ev: string; [k: string]: unknown };
  const listeners = new Map<string, Set<(e: Ev) => void>>();
  engine.onEvent((e) => listeners.get(e.ev)?.forEach((l) => l(e)));
  const on = (name: string, listener: (e: Ev) => void): (() => void) => {
    let set = listeners.get(name);
    if (!set) listeners.set(name, (set = new Set()));
    set.add(listener);
    return () => set.delete(listener);
  };
  let counter = 0;
  const newId = (prefix: string) => `${prefix}${++counter}`;
  const fire = (op: string, args: Record<string, unknown>) => {
    engine.request(op, args).catch((err) => console.warn(`[native voice] ${op}:`, err));
  };

  // ─── Peers ─────────────────────────────────────────────────────────────
  interface SlotRef {
    peerId: string;
    index: number;
  }

  class EnginePeer implements VoicePeer {
    readonly peerId = newId("peer");
    connectionState: RTCPeerConnectionState = "new";
    onicecandidate: ((event: { readonly candidate: VoiceIceCandidate | null }) => void) | null = null;
    onconnectionstatechange: (() => void) | null = null;
    ontrack: ((event: VoiceTrackEvent) => void) | null = null;
    private readonly transceivers: { index: number }[] = [];
    private readonly senders: VoiceSender[] = [];
    // Every operation runs in order: the engine must have the peer before a
    // transceiver, and the transceivers before the offer.
    private queue: Promise<unknown>;
    private readonly unsubscribe: (() => void)[] = [];
    private closed = false;

    constructor(config: RTCConfiguration) {
      this.queue = engine!.request("peer.create", {
        peerId: this.peerId,
        iceServers: (config.iceServers ?? []).map((s) => ({
          urls: Array.isArray(s.urls) ? s.urls : [s.urls],
          username: s.username ?? "",
          credential: typeof s.credential === "string" ? s.credential : "",
        })),
      });
      this.unsubscribe.push(
        on("peer.ice", (e) => {
          if (e.peerId !== this.peerId) return;
          this.onicecandidate?.({
            candidate: {
              candidate: e.candidate as string,
              sdpMid: e.sdpMid as string,
              sdpMLineIndex: e.sdpMLineIndex as number,
              usernameFragment: null,
            },
          });
        }),
        on("peer.state", (e) => {
          if (e.peerId !== this.peerId || this.closed) return;
          this.connectionState = e.state as RTCPeerConnectionState;
          this.onconnectionstatechange?.();
        }),
        on("peer.track", (e) => {
          if (e.peerId !== this.peerId) return;
          const transceiver = this.transceivers[e.index as number];
          if (!transceiver) return;
          const track: SlotRef = { peerId: this.peerId, index: e.index as number };
          this.ontrack?.({ transceiver, track, streams: [] });
        }),
        on("engine.lost", () => {
          if (this.closed) return;
          this.connectionState = "failed";
          this.onconnectionstatechange?.();
        }),
      );
    }

    run<T>(op: () => Promise<T>): Promise<T> {
      const next = this.queue.then(op, op);
      // An operation the engine couldn't do leaves this connection unusable;
      // say so the way a browser connection would, so the client's retries
      // take over.
      this.queue = next.catch((err) => this.fail(err));
      return next;
    }

    private fail(err: unknown): void {
      if (this.closed || this.connectionState === "failed") return;
      console.warn("[native voice] peer failed:", err);
      this.connectionState = "failed";
      this.onconnectionstatechange?.();
    }

    addTransceiver(_kind: "audio", _init: { direction: "recvonly" }): unknown {
      const transceiver = { index: this.transceivers.length };
      this.transceivers.push(transceiver);
      void this.run(() => engine!.request("peer.addRecvOnlyAudio", { peerId: this.peerId }));
      return transceiver;
    }

    getTransceivers(): readonly unknown[] {
      return this.transceivers;
    }

    attachMic(mic: EngineMic): void {
      void this.run(async () => {
        await mic.ensureOpen();
        await engine!.request("peer.attachMic", { peerId: this.peerId, micId: mic.micId });
      });
      this.senders.push({
        track: { kind: "audio" },
        getParameters: () => ({ encodings: [{}] }) as unknown as RTCRtpSendParameters,
        setParameters: async (parameters) => {
          const bps = parameters.encodings?.[0]?.maxBitrate;
          if (bps) await this.run(() => engine!.request("peer.setMaxBitrate", { peerId: this.peerId, bps }));
        },
      });
    }

    getSenders(): readonly VoiceSender[] {
      return this.senders;
    }

    async createOffer(): Promise<RTCSessionDescriptionInit> {
      const sdp = (await this.run(() => engine!.request("peer.createOffer", { peerId: this.peerId }))) as string;
      return { type: "offer", sdp };
    }

    async setLocalDescription(d: RTCSessionDescriptionInit): Promise<void> {
      await this.run(() => engine!.request("peer.setLocal", { peerId: this.peerId, type: d.type, sdp: d.sdp }));
    }

    async setRemoteDescription(d: RTCSessionDescriptionInit): Promise<void> {
      await this.run(() => engine!.request("peer.setRemote", { peerId: this.peerId, type: d.type, sdp: d.sdp }));
    }

    async addIceCandidate(c: RTCIceCandidateInit): Promise<void> {
      if (!c?.candidate) return;
      await this.run(() =>
        engine!.request("peer.addIce", {
          peerId: this.peerId,
          candidate: c.candidate,
          sdpMid: c.sdpMid ?? "0",
          sdpMLineIndex: c.sdpMLineIndex ?? 0,
        }),
      );
    }

    restartIce(): void {
      void this.run(() => engine!.request("peer.restartIce", { peerId: this.peerId }));
    }

    async getStats(): Promise<VoiceStatsReport> {
      const reports = (await this.run(() => engine!.request("peer.getStats", { peerId: this.peerId }))) as unknown[];
      return { forEach: (cb) => (reports ?? []).forEach(cb) };
    }

    close(): void {
      if (this.closed) return;
      this.closed = true;
      this.connectionState = "closed";
      this.unsubscribe.forEach((u) => u());
      void this.run(() => engine!.request("peer.close", { peerId: this.peerId }));
    }
  }

  // ─── Microphone ────────────────────────────────────────────────────────
  class EngineMic implements LocalMic {
    private speaking = false;
    private enabled = true;
    /** The engine restarted under this mic; reopen it before its next use. */
    private lost = false;
    private readonly off: (() => void)[];

    constructor(
      readonly micId: string,
      private options: MicOptions,
    ) {
      this.off = [
        on("mic.level", (e) => {
          if (e.micId === micId) this.speaking = e.speaking === true;
        }),
        on("engine.lost", () => {
          this.lost = true;
          this.speaking = false;
        }),
      ];
    }

    async ensureOpen(): Promise<void> {
      if (!this.lost) return;
      await engine!.request("mic.open", { micId: this.micId, options: this.options });
      if (!this.enabled) await engine!.request("mic.enable", { micId: this.micId, on: false });
      this.lost = false;
    }

    attachTo(peer: VoicePeer): void {
      (peer as EnginePeer).attachMic(this);
    }

    setEnabled(on: boolean): void {
      this.enabled = on;
      fire("mic.enable", { micId: this.micId, on });
    }

    isSpeaking(): boolean {
      return this.speaking;
    }

    async update(options: MicOptions): Promise<void> {
      this.options = options;
      if (this.lost) return;
      await engine!.request("mic.update", { micId: this.micId, options });
    }

    stop(): void {
      this.off.forEach((off) => off());
      fire("mic.close", { micId: this.micId });
    }
  }

  // ─── Backend ───────────────────────────────────────────────────────────
  // Output choice is remembered so a restarted engine gets it again with the
  // first slot of the rebuilt call.
  let output: { deviceId: string; volume: number } | null = null;
  let outputStale = false;
  on("engine.lost", () => (outputStale = true));

  const backend: VoiceMediaBackend = {
    kind: "native",
    noiseSuppressionModes: ["none", "browser", "rnnoise"],
    createPeer: (config) => new EnginePeer(config),
    acquireMic: async (options) => {
      const micId = newId("mic");
      await engine.request("mic.open", { micId, options });
      return new EngineMic(micId, options);
    },
    attachSlot: (slot, event: VoiceTrackEvent) => {
      if (outputStale && output) fire("playout.output", output);
      outputStale = false;
      const ref = event.track as SlotRef;
      fire("playout.attach", { slot, peerId: ref.peerId, index: ref.index });
    },
    setSlotGain: (slot, gain) => fire("playout.gain", { slot, gain }),
    setSlotPosition: (slot, at: WorldPosition) => fire("playout.position", { slot, ...at }),
    setListenerPosition: (at: WorldPosition) => fire("playout.listener", { ...at }),
    setOutput: (options) => {
      output = options;
      fire("playout.output", options);
    },
    closeGraph: () => fire("playout.close", {}),
    listDevices: async () =>
      (await engine.request("devices.list")) as { inputs: AudioDevice[]; outputs: AudioDevice[] },
    onDevicesChanged: (listener) => on("devices.changed", () => listener()),
    startMicTest: async (options, onLevel): Promise<MicTest> => {
      const off = on("test.level", (e) => onLevel(e.level as number));
      await engine.request("test.start", { options, outputDeviceId: options.outputDeviceId });
      return {
        setMonitoring: async (on) => void (await engine.request("test.monitor", { on })),
        setGain: (gain) => fire("test.gain", { gain }),
        stop: () => {
          off();
          onLevel(0);
          fire("test.stop", {});
        },
      };
    },
  };

  const bridge: ChatterDesktopBridge = {
    ...base,
    features: [...features],
    voiceBackend: features.has("voice-backend@1") ? (version: 1) => (version === 1 ? backend : null) : undefined,
    pushToTalk: features.has("ptt")
      ? {
          subscribe: (listener) => on("ptt", (e) => listener(e.down === true)),
          getBinding: () => engine.pttGet(),
          captureBinding: () => engine.pttCapture(),
          clearBinding: () => engine.pttClear(),
        }
      : undefined,
  };
  w.chatterDesktop = Object.freeze(bridge);
}
