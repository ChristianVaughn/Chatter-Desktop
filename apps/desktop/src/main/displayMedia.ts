import { BrowserWindow, desktopCapturer, ipcMain, type DesktopCapturerSource, type Session } from "electron";
import { join } from "node:path";
import { IPC, type CaptureSource } from "../shared/ipc";
import { isServerUrl } from "./origin";
import { isShellUrl, shellPageUrl } from "./shellPages";

interface Pending {
  sources: DesktopCapturerSource[];
  resolve: (source: DesktopCapturerSource | null) => void;
  window: BrowserWindow;
}

let pending: Pending | null = null;

function toCaptureSource(s: DesktopCapturerSource): CaptureSource {
  return {
    id: s.id,
    name: s.name,
    kind: s.id.startsWith("screen:") ? "screen" : "window",
    thumbnail: s.thumbnail.toDataURL(),
    appIcon: s.appIcon && !s.appIcon.isEmpty() ? s.appIcon.toDataURL() : null,
  };
}

/** Show the shell's own picker and resolve with what the user chose. */
function pickSource(parent: BrowserWindow, sources: DesktopCapturerSource[]): Promise<DesktopCapturerSource | null> {
  pending?.resolve(null);
  pending?.window.destroy();

  return new Promise((resolve) => {
    const window = new BrowserWindow({
      parent,
      modal: true,
      width: 760,
      height: 560,
      minWidth: 480,
      minHeight: 360,
      title: "Share your screen",
      show: false,
      autoHideMenuBar: true,
      backgroundColor: "#262626",
      webPreferences: {
        preload: join(__dirname, "../preload/index.js"),
        sandbox: true,
        contextIsolation: true,
      },
    });
    const entry: Pending = {
      sources,
      window,
      resolve: (source) => {
        if (pending === entry) pending = null;
        resolve(source);
      },
    };
    pending = entry;
    window.once("ready-to-show", () => window.show());
    window.on("closed", () => entry.resolve(null));
    void window.loadURL(shellPageUrl("picker"));
  });
}

export function installDisplayMediaHandler(session: Session, getParent: () => BrowserWindow | null): void {
  ipcMain.handle(IPC.pickerGetSources, (event) => {
    if (!isShellUrl(event.senderFrame?.url) || !pending) return [];
    return pending.sources.map(toCaptureSource);
  });

  ipcMain.handle(IPC.pickerChoose, (event, id: unknown) => {
    if (!isShellUrl(event.senderFrame?.url) || !pending) return;
    const entry = pending;
    const source = typeof id === "string" ? (entry.sources.find((s) => s.id === id) ?? null) : null;
    entry.resolve(source);
    entry.window.destroy();
  });

  session.setDisplayMediaRequestHandler(async (request, callback) => {
    const parent = getParent();
    if (!parent || !isServerUrl(request.securityOrigin) || !request.videoRequested) {
      callback(null);
      return;
    }
    try {
      const sources = await desktopCapturer.getSources({
        types: ["screen", "window"],
        thumbnailSize: { width: 320, height: 180 },
        fetchWindowIcons: true,
      });
      const chosen = await pickSource(parent, sources);
      // Video only for now. Electron's Windows loopback would capture the
      // whole system mix, including other people's voices from this call;
      // per-application audio arrives with the native engine.
      callback(chosen ? { video: chosen } : null);
    } catch {
      callback(null);
    }
  });
}
