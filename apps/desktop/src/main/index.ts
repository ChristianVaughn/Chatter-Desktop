import { app, ipcMain, Menu, session, type IpcMainInvokeEvent } from "electron";
import { IPC, type PreloadRole } from "../shared/ipc";
import { installDisplayMediaHandler } from "./displayMedia";
import { engineFeatures, installEngineBridge } from "./engineBridge";
import { engine } from "./engineHost";
import { isServerUrl } from "./origin";
import { installPermissionHandlers } from "./permissions";
import { getServerOrigin, normaliseOrigin, probeServer, setServerOrigin } from "./serverStore";
import { isShellUrl } from "./shellPages";
import { createTray } from "./tray";
import { createMainWindow, getMainWindow, loadHome, markQuitting, showMainWindow, showServerPicker } from "./window";

const APP_ID = "com.chatter.desktop";

// A separate profile (tests, or a second account side by side). Must be set
// before the single-instance lock, which lives in the profile directory.
if (process.env["CHATTER_USER_DATA"]) app.setPath("userData", process.env["CHATTER_USER_DATA"]);

// A second launch hands over to the running instance and exits.
if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on("second-instance", () => showMainWindow());
  start();
}

function start(): void {
  // Join, mute and entrance sounds play without a click having happened first.
  app.commandLine.appendSwitch("autoplay-policy", "no-user-gesture-required");
  if (process.platform === "win32") app.setAppUserModelId(APP_ID);

  app.whenReady().then(async () => {
    app.userAgentFallback = `${app.userAgentFallback} ChatterDesktop/${app.getVersion()}`;

    // The engine starts first so the page's first load already knows which
    // native features it can offer.
    engine.start();
    installEngineBridge(getMainWindow);
    await engine.ready(3000);

    installPermissionHandlers(session.defaultSession);
    installDisplayMediaHandler(session.defaultSession, getMainWindow);
    registerIpc();
    Menu.setApplicationMenu(buildAppMenu());

    createMainWindow();
    createTray({
      show: showMainWindow,
      changeServer: showServerPicker,
      quit: () => app.quit(),
    });
  });

  app.on("before-quit", () => {
    markQuitting();
    engine.stop();
  });
  // The window only hides on close, so this fires on Quit alone.
  app.on("window-all-closed", () => app.quit());
}

function fromShell(event: IpcMainInvokeEvent): boolean {
  return isShellUrl(event.senderFrame?.url);
}

function registerIpc(): void {
  ipcMain.on(IPC.preloadRole, (event) => {
    const url = event.senderFrame?.url;
    let role: PreloadRole = { role: null };
    if (isShellUrl(url)) {
      role = { role: "shell" };
    } else if (isServerUrl(url)) {
      role = {
        role: "remote",
        info: { bridgeVersion: 1, appVersion: app.getVersion(), platform: process.platform, features: engineFeatures() },
      };
    }
    event.returnValue = role;
  });

  ipcMain.on(IPC.remoteFocus, (event) => {
    if (isServerUrl(event.senderFrame?.url)) showMainWindow();
  });

  ipcMain.handle(IPC.getServer, (event) => (fromShell(event) ? getServerOrigin() : null));

  ipcMain.handle(IPC.probeServer, (event, input: unknown) => {
    if (!fromShell(event) || typeof input !== "string") return { ok: false, error: "Not allowed." };
    return probeServer(input);
  });

  ipcMain.handle(IPC.setServer, (event, input: unknown) => {
    if (!fromShell(event) || typeof input !== "string") return;
    const origin = normaliseOrigin(input);
    if (!origin) return;
    setServerOrigin(origin);
    loadHome();
  });

  ipcMain.handle(IPC.retryServer, (event) => {
    if (fromShell(event)) loadHome();
  });

  ipcMain.handle(IPC.changeServer, (event) => {
    if (fromShell(event)) showServerPicker();
  });
}

/** Hidden behind Alt; mostly here so the usual shortcuts work. */
function buildAppMenu(): Menu {
  return Menu.buildFromTemplate([
    {
      label: "Chatter",
      submenu: [
        { label: "Change server…", click: showServerPicker },
        { type: "separator" },
        { role: "quit" },
      ],
    },
    { role: "editMenu" },
    {
      label: "View",
      submenu: [
        { role: "reload" },
        { role: "forceReload" },
        { role: "toggleDevTools" },
        { type: "separator" },
        { role: "resetZoom" },
        { role: "zoomIn" },
        { role: "zoomOut" },
        { type: "separator" },
        { role: "togglefullscreen" },
      ],
    },
    { role: "windowMenu" },
  ]);
}
