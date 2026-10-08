import { contextBridge, ipcRenderer } from "electron";
import { IPC, type PreloadRole, type ShellApi } from "../shared/ipc";

// The main process decides what this page is from its frame URL; the page
// itself has no say. Anything else (a stray iframe, a foreign origin) gets
// nothing.
const role = ipcRenderer.sendSync(IPC.preloadRole) as PreloadRole;

if (role.role === "shell") {
  const api: ShellApi = {
    getServer: () => ipcRenderer.invoke(IPC.getServer),
    probeServer: (input) => ipcRenderer.invoke(IPC.probeServer, input),
    setServer: (origin) => ipcRenderer.invoke(IPC.setServer, origin),
    retryServer: () => ipcRenderer.invoke(IPC.retryServer),
    changeServer: () => ipcRenderer.invoke(IPC.changeServer),
    pickerGetSources: () => ipcRenderer.invoke(IPC.pickerGetSources),
    pickerChoose: (id) => ipcRenderer.invoke(IPC.pickerChoose, id),
  };
  contextBridge.exposeInMainWorld("shellApi", api);
} else if (role.role === "remote") {
  contextBridge.exposeInMainWorld("chatterDesktop", Object.freeze({ ...role.info }));
  contextBridge.exposeInMainWorld("__chatterShell", {
    focus: () => ipcRenderer.send(IPC.remoteFocus),
  });

  // The client raises `new Notification(...)` and calls window.focus() on
  // click, which can't bring back a window hidden in the tray. Wrap the
  // constructor so every click also asks the main process to show the window.
  // Statics (permission, requestPermission) are inherited through the class.
  try {
    contextBridge.executeInMainWorld({
      func: () => {
        const w = window as unknown as {
          Notification?: typeof Notification;
          __chatterShell: { focus(): void };
        };
        const Native = w.Notification;
        if (!Native) return;
        const shell = w.__chatterShell;
        class DesktopNotification extends Native {
          constructor(title: string, options?: NotificationOptions) {
            super(title, options);
            this.addEventListener("click", () => shell.focus());
          }
        }
        w.Notification = DesktopNotification;
      },
    });
  } catch {
    // executeInMainWorld is experimental; without it a click still focuses a
    // visible window, it just can't restore one from the tray.
  }
}
