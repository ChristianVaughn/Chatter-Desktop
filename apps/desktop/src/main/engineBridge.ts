import { BrowserWindow, ipcMain, type WebContents } from "electron";
import { join } from "node:path";
import { PAGE_OPS } from "../shared/engine";
import { IPC } from "../shared/ipc";
import { engine } from "./engineHost";
import { forgetAppAudio, pageAudioStreams } from "./displayMedia";
import { currentGame, onGameChanged } from "./games";
import { isServerUrl } from "./origin";
import { DEFAULT_PTT_BINDING, getPrefs, getPttBinding, setPrefs, setPttBinding, type PttBindingConfig } from "./serverStore";
import { isShellUrl, shellPageUrl } from "./shellPages";

const pageOps = new Set<string>(PAGE_OPS);

interface PttBindingInfo {
  code: string;
  label: string;
}

/** Tell the engine which key to watch; returns it with a display label. */
async function applyPttBinding(binding: PttBindingConfig | null): Promise<PttBindingInfo | null> {
  const value = await engine.request("ptt.set", { binding }).catch(() => null);
  return (value as PttBindingInfo | null) ?? null;
}

/** Features the page may rely on, from what the running engine reports. */
export function engineFeatures(): string[] {
  const hello = engine.hello;
  if (!hello) return [];
  const features: string[] = [];
  if (hello.features.includes("voice")) features.push("voice-backend@1");
  if (hello.features.includes("ptt")) features.push("ptt");
  if (hello.features.includes("app-audio")) features.push("app-audio@1");
  features.push("game-activity");
  if (hello.features.includes("ducking")) features.push("ducking");
  return features;
}

let captureWindow: BrowserWindow | null = null;
let captureResolve: ((code: string | null) => void) | null = null;

/**
 * Asks for the push-to-talk key in a window the shell owns. The page never
 * sees keystrokes: it learns the one key the person chose, after they chose it.
 */
function captureKey(parent: BrowserWindow): Promise<string | null> {
  captureResolve?.(null);
  captureWindow?.destroy();
  return new Promise((resolve) => {
    const window = new BrowserWindow({
      parent,
      modal: true,
      width: 420,
      height: 220,
      resizable: false,
      minimizable: false,
      maximizable: false,
      title: "Push-to-talk key",
      show: false,
      autoHideMenuBar: true,
      backgroundColor: "#262626",
      webPreferences: { preload: join(__dirname, "../preload/index.js"), sandbox: true, contextIsolation: true },
    });
    captureWindow = window;
    const settle = (code: string | null) => {
      if (captureResolve === settle) captureResolve = null;
      resolve(code);
    };
    captureResolve = settle;
    window.once("ready-to-show", () => window.show());
    window.on("closed", () => {
      // Only this capture: a newer one may have replaced it already.
      if (captureWindow === window) captureWindow = null;
      settle(null);
    });
    void window.loadURL(shellPageUrl("keybind"));
  });
}

export function installEngineBridge(getWindow: () => BrowserWindow | null): void {
  // Requests from the Chatter page: only from the user's server, and only the
  // operations listed for pages.
  ipcMain.handle(IPC.engineRequest, async (event, op: unknown, args: unknown) => {
    if (!isServerUrl(event.senderFrame?.url)) throw new Error("not allowed");
    if (typeof op !== "string" || !pageOps.has(op)) throw new Error(`unknown operation ${String(op)}`);
    const payload = args && typeof args === "object" ? (args as Record<string, unknown>) : {};
    // The largest real request is an SDP of a few kilobytes.
    if (JSON.stringify(payload).length > 256 * 1024) throw new Error("request too large");
    if (op === "appaudio.stop") {
      // Only a stream the page was given.
      const stream = Number(payload["stream"]);
      if (!pageAudioStreams.delete(stream)) return null;
    }
    return engine.request(op, payload);
  });

  engine.onEvent((event) => {
    const contents = getWindow()?.webContents;
    if (contents && !contents.isDestroyed() && isServerUrl(contents.getURL())) {
      contents.send(IPC.engineEvent, event);
    }
  });

  engine.onReady(() => {
    void applyPttBinding(getPttBinding());
    void engine.request("ducking.set", { amount: getPrefs().ducking }).catch(() => {});
  });

  ipcMain.handle(IPC.duckingGet, (event) => (isServerUrl(event.senderFrame?.url) ? getPrefs().ducking : 0));
  ipcMain.handle(IPC.duckingSet, async (event, amount: unknown) => {
    if (!isServerUrl(event.senderFrame?.url) || typeof amount !== "number" || !Number.isFinite(amount)) return;
    const clamped = Math.min(1, Math.max(0, amount));
    setPrefs({ ducking: clamped });
    await engine.request("ducking.set", { amount: clamped }).catch(() => {});
  });

  // Game activity goes to the page, which reports it to the server.
  onGameChanged((game) => {
    const contents = getWindow()?.webContents;
    if (contents && !contents.isDestroyed() && isServerUrl(contents.getURL())) {
      contents.send(IPC.engineEvent, { ev: "game", game });
    }
  });
  ipcMain.handle(IPC.gameCurrent, (event) => (isServerUrl(event.senderFrame?.url) ? currentGame() : null));

  // Screen-share audio the page owns: 10 ms frames of 48 kHz stereo float.
  engine.onBinary((stream, data) => {
    if (!pageAudioStreams.has(stream)) return;
    const contents = getWindow()?.webContents;
    if (contents && !contents.isDestroyed() && isServerUrl(contents.getURL())) {
      contents.send(IPC.engineAudio, stream, data);
    }
  });
  engine.onEvent((event) => {
    if (event.ev === "engine.lost") forgetAppAudio();
  });

  ipcMain.handle(IPC.pttGet, async (event) => {
    if (!isServerUrl(event.senderFrame?.url)) return null;
    return applyPttBinding(getPttBinding());
  });

  ipcMain.handle(IPC.pttCapture, async (event) => {
    if (!isServerUrl(event.senderFrame?.url)) return null;
    const parent = getWindow();
    if (!parent) return null;
    const code = await captureKey(parent);
    if (!code) return applyPttBinding(getPttBinding());
    setPttBinding({ code });
    return applyPttBinding({ code });
  });

  ipcMain.handle(IPC.pttClear, async (event) => {
    if (!isServerUrl(event.senderFrame?.url)) return;
    setPttBinding(null);
    await applyPttBinding(DEFAULT_PTT_BINDING);
  });

  // From the shell's capture window.
  ipcMain.handle(IPC.keybindDone, (event, code: unknown) => {
    if (!isShellUrl(event.senderFrame?.url)) return;
    const resolve = captureResolve;
    captureResolve = null;
    resolve?.(typeof code === "string" && /^[A-Za-z0-9]{1,24}$/.test(code) ? code : null);
    captureWindow?.destroy();
  });
}

/** Everything the engine holds for a page goes when that page does. */
export function resetEngineOnNavigation(contents: WebContents): void {
  const reset = () => {
    forgetAppAudio();
    void engine.request("session.reset").catch(() => {});
  };
  // Once a new page has actually replaced this one: a navigation that starts
  // and never commits (a download, a 204, a blocked link) leaves the call's
  // page, and its call, in place.
  contents.on("did-navigate", reset);
  contents.on("render-process-gone", reset);
}
