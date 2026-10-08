import { BrowserWindow, screen } from "electron";
import { join } from "node:path";
import icon from "../../resources/icon.png?asset";
import { applyUnreadCount, unreadFromTitle } from "./badge";
import { attachContextMenu } from "./contextMenu";
import { attachNavigationLock } from "./navLock";
import { getServerOrigin, getWindowBounds, setWindowBounds } from "./serverStore";
import { shellPageUrl } from "./shellPages";
import { setTrayUnread } from "./tray";

let mainWindow: BrowserWindow | null = null;
let quitting = false;

export const getMainWindow = () => mainWindow;
export const markQuitting = () => {
  quitting = true;
};

/** Saved bounds, unless the display they were on has since gone away. */
function initialBounds() {
  const saved = getWindowBounds();
  if (!saved || saved.x === undefined || saved.y === undefined) return saved;
  const visible = screen.getAllDisplays().some(({ workArea: a }) => {
    return saved.x! < a.x + a.width && saved.x! + saved.width > a.x && saved.y! < a.y + a.height && saved.y! + 40 > a.y;
  });
  return visible ? saved : { width: saved.width, height: saved.height, maximized: saved.maximized };
}

export function createMainWindow(): BrowserWindow {
  const bounds = initialBounds();
  const window = new BrowserWindow({
    width: bounds?.width ?? 1280,
    height: bounds?.height ?? 800,
    x: bounds?.x,
    y: bounds?.y,
    minWidth: 940,
    minHeight: 560,
    title: "Chatter",
    icon,
    show: false,
    autoHideMenuBar: true,
    backgroundColor: "#262626",
    webPreferences: {
      preload: join(__dirname, "../preload/index.js"),
      sandbox: true,
      contextIsolation: true,
      spellcheck: true,
      // Voice, speaking indicators and the WebSocket keep running while the
      // window is hidden in the tray.
      backgroundThrottling: false,
    },
  });
  mainWindow = window;
  if (bounds?.maximized) window.maximize();

  window.once("ready-to-show", () => window.show());
  window.on("focus", () => window.flashFrame(false));

  // Close hides to the tray; quitting goes through the tray or app menu.
  window.on("close", (event) => {
    if (quitting) return;
    event.preventDefault();
    window.hide();
  });

  let saveTimer: NodeJS.Timeout | undefined;
  const saveBounds = () => {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      if (window.isDestroyed() || window.isMinimized()) return;
      const maximized = window.isMaximized();
      const b = maximized ? window.getNormalBounds() : window.getBounds();
      setWindowBounds({ x: b.x, y: b.y, width: b.width, height: b.height, maximized });
    }, 500);
  };
  window.on("resize", saveBounds);
  window.on("move", saveBounds);
  window.on("maximize", saveBounds);
  window.on("unmaximize", saveBounds);

  const contents = window.webContents;
  attachNavigationLock(contents);
  attachContextMenu(contents);

  contents.on("page-title-updated", (_event, title) => {
    const unread = unreadFromTitle(title);
    applyUnreadCount(window, unread);
    setTrayUnread(unread);
  });

  contents.on("did-fail-load", (_event, code, description, url, isMainFrame) => {
    // -3 is ERR_ABORTED: a navigation replaced by another, not a failure.
    if (!isMainFrame || code === -3) return;
    void contents.loadURL(shellPageUrl("offline", { url, error: description }));
  });

  contents.on("render-process-gone", (_event, details) => {
    if (details.reason === "clean-exit") return;
    void contents.loadURL(shellPageUrl("offline", { url: getServerOrigin() ?? "", error: `The page stopped (${details.reason}).` }));
  });

  loadHome();
  return window;
}

/** The server if one is set, otherwise the picker. */
export function loadHome(): void {
  const origin = getServerOrigin();
  if (!mainWindow) return;
  void mainWindow.loadURL(origin ? `${origin}/` : shellPageUrl("server"));
}

export function showServerPicker(): void {
  if (!mainWindow) return;
  showMainWindow();
  void mainWindow.loadURL(shellPageUrl("server", { change: "1" }));
}

export function showMainWindow(): void {
  if (!mainWindow) return;
  if (mainWindow.isMinimized()) mainWindow.restore();
  mainWindow.show();
  mainWindow.focus();
}
