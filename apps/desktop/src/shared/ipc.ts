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
  settingsGet: "settings:get",
  settingsSet: "settings:set",
  runningApps: "settings:running-apps",
  // Remote pages (served by the user's Chatter server)
  remoteFocus: "remote:focus",
  engineRequest: "engine:request",
  engineEvent: "engine:event",
  pttGet: "ptt:get",
  pttCapture: "ptt:capture",
  pttClear: "ptt:clear",
  appAudioClaim: "appaudio:claim",
  gameCurrent: "game:current",
  duckingGet: "ducking:get",
  duckingSet: "ducking:set",
  engineAudio: "engine:audio",
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

/** The audio to go with a screen share, as chosen in the picker. */
export interface AudioChoice {
  kind: "none" | "window" | "app" | "system";
  pid?: number;
}

export interface PickerData {
  sources: CaptureSource[];
  /** Null when this machine can't capture app audio (no engine, or Windows
   *  before 10 build 20348). */
  audio: {
    perApp: boolean;
    allExcept: boolean;
    /** A shared window can be traced to its app (Windows). */
    windowApp: boolean;
    apps: { pid: number; name: string }[];
  } | null;
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

/** The desktop preferences as the Settings window sees them. */
export interface DesktopPrefsView {
  startWithSystem: boolean;
  startMinimized: boolean;
  shareGameActivity: boolean;
  extraGames: { exe: string; name: string }[];
  autoUpdate: boolean;
  appVersion: string;
}

/** API the preload exposes to the shell's own pages as `window.shellApi`. */
export interface ShellApi {
  getServer(): Promise<string | null>;
  probeServer(input: string): Promise<ProbeResult>;
  setServer(origin: string): Promise<void>;
  retryServer(): Promise<void>;
  changeServer(): Promise<void>;
  pickerGetSources(): Promise<PickerData>;
  pickerChoose(id: string | null, audio?: AudioChoice): Promise<void>;
  keybindDone(code: string | null): Promise<void>;
  settingsGet(): Promise<DesktopPrefsView>;
  settingsSet(update: Partial<DesktopPrefsView>): Promise<DesktopPrefsView>;
  runningApps(): Promise<{ exe: string; name: string }[]>;
}
