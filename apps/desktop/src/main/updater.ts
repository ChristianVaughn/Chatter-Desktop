import { app, dialog, Notification, session } from "electron";
import electronUpdater from "electron-updater";
import { engine } from "./engineHost";
import { flushSession } from "./persistence";
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

/**
 * quitAndInstall starts the installer straight away, and the installer kills
 * whatever of Chatter is still running about 1.3 s later. So do what quitting
 * has to do first: put the sign-in on disk and stop the engine.
 */
async function restartToUpdate(): Promise<void> {
  await Promise.allSettled([flushSession(session.defaultSession), engine.shutdown()]);
  autoUpdater.quitAndInstall();
}

let listening = false;
let started = false;
let downloaded: string | null = null;

/** Shared by the background checks and "Check for updates…". */
function init(): void {
  if (listening) return;
  // The setting only governs the background checks; an update someone asked
  // for downloads and installs on quit.
  autoUpdater.autoDownload = true;
  autoUpdater.autoInstallOnAppQuit = true;
  listening = true;
  autoUpdater.on("error", (err) => console.warn("[updater]", err.message));
  autoUpdater.on("update-downloaded", (info) => {
    downloaded = info.version;
    const note = new Notification({
      title: "Chatter update ready",
      body: `Version ${info.version} installs when you quit Chatter. Click to restart now.`,
    });
    note.on("click", () => void restartToUpdate());
    note.show();
  });
}

export function startUpdater(): void {
  if (started || !canUpdate()) return;
  started = true;
  const check = () => {
    // Read each time: the setting can change while the app runs.
    if (!getPrefs().autoUpdate) return;
    init();
    void autoUpdater.checkForUpdates().catch(() => {});
  };
  check();
  setInterval(check, 6 * 60 * 60 * 1000);
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
    if (response === 0) void restartToUpdate();
    return;
  }
  init();
  try {
    const result = await autoUpdater.checkForUpdates();
    const latest = result?.updateInfo.version;
    if (!latest || latest === app.getVersion()) say("Chatter is up to date.", `Version ${app.getVersion()}.`);
    else say(`Downloading version ${latest}…`, "You'll be told when it's ready.");
  } catch (err) {
    say("Couldn't check for updates.", err instanceof Error ? err.message : String(err));
  }
}
