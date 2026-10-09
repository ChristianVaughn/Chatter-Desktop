import { app, BrowserWindow, ipcMain } from "electron";
import { join } from "node:path";
import { IPC, type DesktopPrefsView } from "../shared/ipc";
import { setStartWithSystem } from "./autostart";
import { engine } from "./engineHost";
import { rescanGames } from "./games";
import { getPrefs, setPrefs, type DesktopPrefs } from "./serverStore";
import { isShellUrl, shellPageUrl } from "./shellPages";
import { startUpdater } from "./updater";

let window: BrowserWindow | null = null;

function view(): DesktopPrefsView {
  return { ...getPrefs(), appVersion: app.getVersion() };
}

export function showSettings(): void {
  if (window && !window.isDestroyed()) {
    window.show();
    window.focus();
    return;
  }
  window = new BrowserWindow({
    width: 560,
    height: 640,
    minWidth: 420,
    minHeight: 420,
    title: "Chatter settings",
    show: false,
    autoHideMenuBar: true,
    backgroundColor: "#262626",
    webPreferences: { preload: join(__dirname, "../preload/index.js"), sandbox: true, contextIsolation: true },
  });
  window.once("ready-to-show", () => window?.show());
  window.on("closed", () => (window = null));
  void window.loadURL(shellPageUrl("settings"));
}

export function installSettings(): void {
  ipcMain.handle(IPC.settingsGet, (event) => (isShellUrl(event.senderFrame?.url) ? view() : null));

  ipcMain.handle(IPC.settingsSet, (event, update: unknown) => {
    if (!isShellUrl(event.senderFrame?.url) || !update || typeof update !== "object") return view();
    const u = update as Partial<DesktopPrefs>;
    const clean: Partial<DesktopPrefs> = {};
    for (const key of ["startWithSystem", "startMinimized", "shareGameActivity", "autoUpdate"] as const) {
      if (typeof u[key] === "boolean") clean[key] = u[key];
    }
    if (Array.isArray(u.extraGames)) {
      clean.extraGames = u.extraGames
        .filter((g) => typeof g?.exe === "string" && typeof g?.name === "string")
        .map((g) => ({ exe: g.exe.slice(0, 260), name: g.name.slice(0, 128) }));
    }
    const prefs = setPrefs(clean);
    if (clean.startWithSystem !== undefined) setStartWithSystem(prefs.startWithSystem);
    if (clean.shareGameActivity !== undefined || clean.extraGames) rescanGames();
    if (clean.autoUpdate) startUpdater();
    return view();
  });

  ipcMain.handle(IPC.runningApps, async (event) => {
    if (!isShellUrl(event.senderFrame?.url)) return [];
    const list = (await engine.request("processes.list").catch(() => [])) as { exe: string; name: string }[];
    return list.map(({ exe, name }) => ({ exe, name }));
  });
}
