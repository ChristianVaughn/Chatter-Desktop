// Channel names and payload shapes shared by the main process and the preload.

export const IPC = {
  /** Sync: what the preload should expose to the page asking. */
  preloadRole: "preload:role",
  // Shell pages (trusted, bundled with the app)
  getServer: "shell:get-server",
  probeServer: "shell:probe-server",
  setServer: "shell:set-server",
  retryServer: "shell:retry-server",
  changeServer: "shell:change-server",
  pickerGetSources: "picker:get-sources",
  pickerChoose: "picker:choose",
  keybindDone: "keybind:done",
  // Remote pages (served by the user's Chatter server)
  remoteFocus: "remote:focus",
  engineRequest: "engine:request",
  engineEvent: "engine:event",
  pttGet: "ptt:get",
  pttCapture: "ptt:capture",
  pttClear: "ptt:clear",
} as const;

export type PreloadRole =
  | { role: "remote"; info: BridgeInfo }
  | { role: "shell" }
  | { role: null };

export interface ProbeResult {
  ok: boolean;
  /** Normalised origin, e.g. "https://chat.example.com". */
  origin?: string;
  error?: string;
}

export interface CaptureSource {
  id: string;
  name: string;
  kind: "screen" | "window";
  thumbnail: string;
  appIcon: string | null;
}

/** What `window.chatterDesktop` exposes to the Chatter client. Additive only. */
export interface BridgeInfo {
  bridgeVersion: 1;
  appVersion: string;
  platform: string;
  /** Capabilities the client may rely on. Empty until a feature ships. */
  features: string[];
}

/** API the preload exposes to the shell's own pages as `window.shellApi`. */
export interface ShellApi {
  getServer(): Promise<string | null>;
  probeServer(input: string): Promise<ProbeResult>;
  setServer(origin: string): Promise<void>;
  retryServer(): Promise<void>;
  changeServer(): Promise<void>;
  pickerGetSources(): Promise<CaptureSource[]>;
  pickerChoose(id: string | null): Promise<void>;
  keybindDone(code: string | null): Promise<void>;
}
