// The protocol between the desktop app and chatter-engine (crates/chatter-engine).
//
// Transport: the engine's stdin/stdout, as frames of
//   u32 little-endian length | u8 kind | payload
// where kind is 'J' (0x4A, UTF-8 JSON) or 'B' (0x42, binary: u32 LE stream id
// then bytes). Requests carry an `id`; the engine answers each with a
// response carrying `re`. Events carry `ev` and are unsolicited.
//
// Keep in step with crates/chatter-engine/src/protocol.rs.

/** Operations the page may ask for. Anything else is refused by the main process. */
export const PAGE_OPS = [
  "devices.list",
  "mic.open",
  "mic.update",
  "mic.enable",
  "mic.close",
  "peer.create",
  "peer.addRecvOnlyAudio",
  "peer.attachMic",
  "peer.createOffer",
  "peer.setLocal",
  "peer.setRemote",
  "peer.addIce",
  "peer.restartIce",
  "peer.getStats",
  "peer.setMaxBitrate",
  "peer.close",
  "playout.attach",
  "playout.gain",
  "playout.position",
  "playout.listener",
  "playout.output",
  "playout.close",
  "test.start",
  "test.monitor",
  "test.gain",
  "test.stop",
] as const;

export type PageOp = (typeof PAGE_OPS)[number];

export interface EngineRequest {
  id: number;
  op: string;
  [arg: string]: unknown;
}

export interface EngineResponse {
  re: number;
  ok: boolean;
  value?: unknown;
  error?: string;
}

export interface EngineEvent {
  ev: string;
  [field: string]: unknown;
}

export interface EngineHello {
  version: string;
  /** "voice" when the media stack initialised; "ptt" when a hotkey backend is available. */
  features: string[];
  hotkeyBackend: string;
}

export const FRAME_JSON = 0x4a;
export const FRAME_BINARY = 0x42;
