import { BrowserWindow, ipcMain, type WebContents } from "electron";
import { join } from "node:path";
import { PAGE_OPS } from "../shared/engine";
import { IPC } from "../shared/ipc";
import { engine } from "./engineHost";
import { isServerUrl } from "./origin";
import { DEFAULT_PTT_BINDING, getPttBinding, setPttBinding, type PttBindingConfig } from "./serverStore";
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
    captureResolve = (code) => {
      captureResolve = null;
      resolve(code);
    };
    window.once("ready-to-show", () => window.show());
    window.on("closed", () => {
      captureWindow = null;
      captureResolve?.(null);
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
    return engine.request(op, payload);
  });

  engine.onEvent((event) => {
    const contents = getWindow()?.webContents;
    if (contents && !contents.isDestroyed() && isServerUrl(contents.getURL())) {
      contents.send(IPC.engineEvent, event);
    }
  });

  engine.onReady(() => void applyPttBinding(getPttBinding()));

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
  const reset = () => void engine.request("session.reset").catch(() => {});
  contents.on("did-start-navigation", (details) => {
    if (details.isMainFrame && !details.isSameDocument) reset();
  });
  contents.on("render-process-gone", reset);
}
