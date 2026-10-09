import { BrowserWindow, desktopCapturer, ipcMain, type DesktopCapturerSource, type Session } from "electron";
import { join } from "node:path";
import { IPC, type AudioChoice, type CaptureSource, type PickerData } from "../shared/ipc";
import { engine } from "./engineHost";
import { isServerUrl } from "./origin";
import { isShellUrl, shellPageUrl } from "./shellPages";

interface Choice {
  source: DesktopCapturerSource;
  audio: AudioChoice;
}

interface Pending {
  sources: DesktopCapturerSource[];
  data: PickerData;
  resolve: (choice: Choice | null) => void;
  window: BrowserWindow;
}

let pending: Pending | null = null;

/** The app-audio stream started for the last share, waiting for the page. */
let unclaimedAudio: number | null = null;
/** Streams the page owns; their audio frames are forwarded to it. */
export const pageAudioStreams = new Set<number>();

/** The page went away or the engine restarted (its captures went with it,
 *  and stream ids start over): nothing is waiting to be collected. */
export function forgetAppAudio(): void {
  unclaimedAudio = null;
  pageAudioStreams.clear();
}

function toCaptureSource(s: DesktopCapturerSource): CaptureSource {
  return {
    id: s.id,
    name: s.name,
    kind: s.id.startsWith("screen:") ? "screen" : "window",
    thumbnail: s.thumbnail.toDataURL(),
    appIcon: s.appIcon && !s.appIcon.isEmpty() ? s.appIcon.toDataURL() : null,
  };
}

/** Our own processes, kept out of "everything" and out of the app list: the
 *  call's audio must never be shared back into the call. */
function ownPids(): number[] {
  return [process.pid, engine.pid].filter((p): p is number => typeof p === "number");
}

/** What the picker can offer for audio on this machine, if anything. */
async function audioOptions(): Promise<PickerData["audio"]> {
  if (!engine.hello?.features.includes("app-audio")) return null;
  try {
    const [caps, apps] = await Promise.all([
      engine.request("appaudio.caps") as Promise<{ per_app: boolean; all_except: boolean }>,
      engine.request("appaudio.list", { excludePids: ownPids() }) as Promise<{ pid: number; name: string }[]>,
    ]);
    return { perApp: caps.per_app, allExcept: caps.all_except, windowApp: caps.per_app && process.platform === "win32", apps };
  } catch {
    return null;
  }
}

/** Show the shell's own picker and resolve with what the user chose. */
function pick(parent: BrowserWindow, sources: DesktopCapturerSource[], data: PickerData): Promise<Choice | null> {
  pending?.resolve(null);
  pending?.window.destroy();

  return new Promise((resolve) => {
    const window = new BrowserWindow({
      parent,
      modal: true,
      width: 760,
      height: 600,
      minWidth: 480,
      minHeight: 380,
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
      data,
      window,
      resolve: (choice) => {
        if (pending === entry) pending = null;
        resolve(choice);
      },
    };
    pending = entry;
    window.once("ready-to-show", () => window.show());
    window.on("closed", () => entry.resolve(null));
    void window.loadURL(shellPageUrl("picker"));
  });
}

/** Start capturing the chosen audio; the stream id, or null for none. */
async function startAudio(choice: Choice): Promise<number | null> {
  const audio = choice.audio;
  let target: Record<string, unknown> | null = null;
  if (audio.kind === "app" && audio.pid) {
    target = { kind: "app", pid: audio.pid };
  } else if (audio.kind === "window") {
    const pid = (await engine.request("appaudio.windowPid", { sourceId: choice.source.id }).catch(() => null)) as number | null;
    if (pid && !ownPids().includes(pid)) target = { kind: "app", pid };
  } else if (audio.kind === "system") {
    target = { kind: "allExcept", pids: ownPids() };
  }
  if (!target) return null;
  try {
    const { stream } = (await engine.request("appaudio.start", { target })) as { stream: number };
    return stream;
  } catch (err) {
    console.warn("[screen share] audio capture failed:", err);
    return null;
  }
}

export function installDisplayMediaHandler(session: Session, getParent: () => BrowserWindow | null): void {
  ipcMain.handle(IPC.pickerGetSources, (event) => {
    if (!isShellUrl(event.senderFrame?.url) || !pending) return { sources: [], audio: null };
    return { ...pending.data, sources: pending.sources.map(toCaptureSource) } satisfies PickerData;
  });

  ipcMain.handle(IPC.pickerChoose, (event, id: unknown, audio: unknown) => {
    if (!isShellUrl(event.senderFrame?.url) || !pending) return;
    const entry = pending;
    const source = typeof id === "string" ? entry.sources.find((s) => s.id === id) : undefined;
    const kind = (audio as AudioChoice | undefined)?.kind;
    const safeAudio: AudioChoice =
      kind === "app" || kind === "window" || kind === "system"
        ? { kind, pid: Number((audio as AudioChoice).pid) || undefined }
        : { kind: "none" };
    entry.resolve(source ? { source, audio: safeAudio } : null);
    entry.window.destroy();
  });

  // The page collects the audio that goes with the share it just started.
  ipcMain.handle(IPC.appAudioClaim, (event) => {
    if (!isServerUrl(event.senderFrame?.url)) return null;
    const stream = unclaimedAudio;
    unclaimedAudio = null;
    if (stream !== null) pageAudioStreams.add(stream);
    return stream;
  });

  session.setDisplayMediaRequestHandler(async (request, callback) => {
    const parent = getParent();
    if (!parent || !isServerUrl(request.securityOrigin) || !request.videoRequested) {
      callback(null);
      return;
    }
    try {
      const [sources, audio] = await Promise.all([
        desktopCapturer.getSources({
          types: ["screen", "window"],
          thumbnailSize: { width: 320, height: 180 },
          fetchWindowIcons: true,
        }),
        audioOptions(),
      ]);
      const choice = await pick(parent, sources, { sources: [], audio });
      if (!choice) {
        callback(null);
        return;
      }
      // Audio comes from the engine, per app, rather than Chromium's
      // loopback, which would also capture everyone in the call.
      if (unclaimedAudio !== null) void engine.request("appaudio.stop", { stream: unclaimedAudio }).catch(() => {});
      unclaimedAudio = await startAudio(choice);
      callback({ video: choice.source });
    } catch {
      callback(null);
    }
  });
}
