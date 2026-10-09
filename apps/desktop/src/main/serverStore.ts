import { app } from "electron";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import type { ProbeResult } from "../shared/ipc";

export interface WindowBounds {
  x?: number;
  y?: number;
  width: number;
  height: number;
  maximized: boolean;
}

/** A push-to-talk key as a DOM `KeyboardEvent.code`, or "Mouse3"–"Mouse5". */
export interface PttBindingConfig {
  code: string;
}

/** Preferences the shell's own Settings window edits. */
export interface DesktopPrefs {
  /** Launch when the user signs in to their computer. */
  startWithSystem: boolean;
  /** Launched that way, stay in the tray instead of opening the window. */
  startMinimized: boolean;
  /** Tell people which game you're playing. */
  shareGameActivity: boolean;
  /** Executables the user added as games, beyond the built-in list. */
  extraGames: { exe: string; name: string }[];
  /** Check for and install updates. */
  autoUpdate: boolean;
  /** How far to turn other apps down while people talk: 0 (off) to 1. */
  ducking: number;
}

export const DEFAULT_PREFS: DesktopPrefs = {
  startWithSystem: false,
  startMinimized: true,
  shareGameActivity: true,
  extraGames: [],
  autoUpdate: true,
  ducking: 0,
};

interface Config {
  serverOrigin?: string;
  window?: WindowBounds;
  pttBinding?: PttBindingConfig | null;
  prefs?: Partial<DesktopPrefs>;
}

const configPath = () => join(app.getPath("userData"), "config.json");

let cache: Config | null = null;

function load(): Config {
  if (cache) return cache;
  try {
    cache = JSON.parse(readFileSync(configPath(), "utf8")) as Config;
  } catch {
    cache = {};
  }
  return cache;
}

function save(): void {
  const path = configPath();
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(load(), null, 2));
}

export function getServerOrigin(): string | null {
  return load().serverOrigin ?? null;
}

export function setServerOrigin(origin: string): void {
  load().serverOrigin = origin;
  save();
}

export function getPrefs(): DesktopPrefs {
  return { ...DEFAULT_PREFS, ...load().prefs };
}

export function setPrefs(update: Partial<DesktopPrefs>): DesktopPrefs {
  load().prefs = { ...getPrefs(), ...update };
  save();
  return getPrefs();
}

export const DEFAULT_PTT_BINDING: PttBindingConfig = { code: "Backquote" };

/** The saved push-to-talk key; the web client's backtick until changed. */
export function getPttBinding(): PttBindingConfig {
  return load().pttBinding ?? DEFAULT_PTT_BINDING;
}

export function setPttBinding(binding: PttBindingConfig | null): void {
  load().pttBinding = binding;
  save();
}

export function getWindowBounds(): WindowBounds | undefined {
  return load().window;
}

export function setWindowBounds(bounds: WindowBounds): void {
  load().window = bounds;
  save();
}

/**
 * Turn whatever the user typed into an origin. A bare host gets https://,
 * except localhost-style hosts, which a dev server usually serves over http.
 */
export function normaliseOrigin(input: string): string | null {
  let text = input.trim();
  if (!text) return null;
  if (!/^https?:\/\//i.test(text)) {
    const local = /^(localhost|127\.0\.0\.1|\[::1\])(:\d+)?(\/|$)/i.test(text);
    text = `${local ? "http" : "https"}://${text}`;
  }
  try {
    const url = new URL(text);
    if (url.protocol !== "http:" && url.protocol !== "https:") return null;
    return url.origin;
  } catch {
    return null;
  }
}

/** Confirm a Chatter server answers at this origin before committing to it. */
export async function probeServer(input: string): Promise<ProbeResult> {
  const origin = normaliseOrigin(input);
  if (!origin) return { ok: false, error: "That doesn't look like a web address." };
  try {
    const res = await fetch(`${origin}/api/version`, { signal: AbortSignal.timeout(8000) });
    if (!res.ok) return { ok: false, origin, error: `Server answered ${res.status}.` };
    const body = (await res.json()) as { version?: unknown };
    if (typeof body.version !== "string") {
      return { ok: false, origin, error: "That server doesn't look like Chatter." };
    }
    return { ok: true, origin };
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    return { ok: false, origin, error: `Couldn't reach the server (${message}).` };
  }
}
