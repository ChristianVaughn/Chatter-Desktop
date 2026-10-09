import { contextBridge, ipcRenderer } from "electron";
import { IPC, type PreloadRole, type ShellApi } from "../shared/ipc";
import { installDesktopBridge, type EngineChannel } from "./mainWorld";

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
    pickerChoose: (id, audio) => ipcRenderer.invoke(IPC.pickerChoose, id, audio),
    keybindDone: (code) => ipcRenderer.invoke(IPC.keybindDone, code),
    settingsGet: () => ipcRenderer.invoke(IPC.settingsGet),
    settingsSet: (update) => ipcRenderer.invoke(IPC.settingsSet, update),
    runningApps: () => ipcRenderer.invoke(IPC.runningApps),
  };
  contextBridge.exposeInMainWorld("shellApi", api);
} else if (role.role === "remote") {
  // The channel to chatter-engine. The main process checks every request's
  // origin and operation; this only carries them.
  const eventListeners = new Set<(event: { ev: string }) => void>();
  ipcRenderer.on(IPC.engineEvent, (_e, event: { ev: string }) => eventListeners.forEach((l) => l(event)));
  const audioListeners = new Set<(stream: number, samples: ArrayBuffer) => void>();
  ipcRenderer.on(IPC.engineAudio, (_e, stream: number, data: Uint8Array) => {
    // A fresh, aligned copy: contextBridge hands ArrayBuffers across by copy.
    const samples = data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) as ArrayBuffer;
    audioListeners.forEach((l) => l(stream, samples));
  });
  const channel: EngineChannel = {
    request: (op, args) => ipcRenderer.invoke(IPC.engineRequest, op, args ?? {}),
    onEvent: (listener) => void eventListeners.add(listener),
    pttGet: () => ipcRenderer.invoke(IPC.pttGet),
    pttCapture: () => ipcRenderer.invoke(IPC.pttCapture),
    pttClear: () => ipcRenderer.invoke(IPC.pttClear),
    appAudioClaim: () => ipcRenderer.invoke(IPC.appAudioClaim),
    gameCurrent: () => ipcRenderer.invoke(IPC.gameCurrent),
    duckingGet: () => ipcRenderer.invoke(IPC.duckingGet),
    duckingSet: (amount) => ipcRenderer.invoke(IPC.duckingSet, amount),
    onAudio: (listener) => void audioListeners.add(listener),
  };
  contextBridge.exposeInMainWorld("__chatterEngine", channel);
  contextBridge.exposeInMainWorld("__chatterShell", {
    focus: () => ipcRenderer.send(IPC.remoteFocus),
  });

  // window.chatterDesktop, built in the page's world so the voice backend's
  // objects behave like the browser objects they stand in for.
  try {
    contextBridge.executeInMainWorld({ func: installDesktopBridge, args: [role.info] });
  } catch (err) {
    console.error("[desktop] could not install the bridge", err);
  }

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
