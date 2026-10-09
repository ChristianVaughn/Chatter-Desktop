import { app, dialog, Notification } from "electron";
import electronUpdater from "electron-updater";
import { getPrefs } from "./serverStore";
import { getMainWindow } from "./window";

const { autoUpdater } = electronUpdater;

/**
 * Updates for the app itself (the shell and the engine). The Chatter UI isn't
 * part of this: it comes from the server and is always the server's version.
 *
 * Only packaged builds that can replace themselves update: the NSIS install
 * on Windows and the AppImage on Linux. A .deb is updated by its package
 * manager.
 */
function canUpdate(): boolean {
  if (!app.isPackaged || process.env["CHATTER_DISABLE_UPDATES"]) return false;
  if (process.platform === "linux" && !process.env["APPIMAGE"]) return false;
  return true;
}

let started = false;
let downloaded: string | null = null;

export function startUpdater(): void {
  if (started || !canUpdate() || !getPrefs().autoUpdate) return;
  started = true;
  autoUpdater.autoDownload = true;
  autoUpdater.autoInstallOnAppQuit = true;
  autoUpdater.on("error", (err) => console.warn("[updater]", err.message));
  autoUpdater.on("update-downloaded", (info) => {
    downloaded = info.version;
    const note = new Notification({
      title: "Chatter update ready",
      body: `Version ${info.version} installs when you quit Chatter. Click to restart now.`,
    });
    note.on("click", () => autoUpdater.quitAndInstall());
    note.show();
  });
  void autoUpdater.checkForUpdates().catch(() => {});
  setInterval(() => void autoUpdater.checkForUpdates().catch(() => {}), 6 * 60 * 60 * 1000);
}

/** The tray's "Check for updates…": answers either way. */
export async function checkForUpdatesNow(): Promise<void> {
  const window = getMainWindow() ?? undefined;
  const say = (message: string, detail?: string) =>
    void (window
      ? dialog.showMessageBox(window, { type: "info", title: "Chatter", message, detail })
      : dialog.showMessageBox({ type: "info", title: "Chatter", message, detail }));
  if (!canUpdate()) {
    say("Updates aren't available for this install.", app.isPackaged ? "Use your package manager to update." : "This is a development build.");
    return;
  }
  if (downloaded) {
    const { response } = await dialog.showMessageBox({
      type: "info",
      title: "Chatter",
      message: `Version ${downloaded} is ready.`,
      buttons: ["Restart now", "Later"],
    });
    if (response === 0) autoUpdater.quitAndInstall();
    return;
  }
  try {
    const result = await autoUpdater.checkForUpdates();
    const latest = result?.updateInfo.version;
    if (!latest || latest === app.getVersion()) say("Chatter is up to date.", `Version ${app.getVersion()}.`);
    else say(`Downloading version ${latest}…`, "You'll be told when it's ready.");
  } catch (err) {
    say("Couldn't check for updates.", err instanceof Error ? err.message : String(err));
  }
}
