import { app, dialog } from "electron";
import { getServerOrigin, setServerOrigin } from "./serverStore";
import { getMainWindow, showMainWindow } from "./window";

/**
 * `chatter://open?url=<page>` opens a page of a Chatter server in the app —
 * an invite, a message link, a theme share. A link to a different server
 * asks before switching to it.
 */
export const SCHEME = "chatter";

export function registerProtocol(): void {
  // In development the scheme would launch electron.exe without our app path.
  if (process.defaultApp) {
    if (process.argv[1]) app.setAsDefaultProtocolClient(SCHEME, process.execPath, [process.argv[1]]);
  } else {
    app.setAsDefaultProtocolClient(SCHEME);
  }
}

/** The page a deep link points at, or null if it isn't one we open. */
export function targetOf(link: string): URL | null {
  try {
    const url = new URL(link);
    if (url.protocol !== `${SCHEME}:`) return null;
    const page = new URL(url.searchParams.get("url") ?? "");
    return page.protocol === "https:" || page.protocol === "http:" ? page : null;
  } catch {
    return null;
  }
}

export function findDeepLink(argv: readonly string[]): string | undefined {
  return argv.find((a) => a.startsWith(`${SCHEME}://`));
}

export async function openDeepLink(link: string): Promise<void> {
  const page = targetOf(link);
  const window = getMainWindow();
  if (!page || !window) return;
  showMainWindow();
  const current = getServerOrigin();
  if (current !== page.origin) {
    const { response } = await dialog.showMessageBox(window, {
      type: "question",
      buttons: ["Switch server", "Cancel"],
      defaultId: 0,
      cancelId: 1,
      title: "Open link",
      message: `Open ${page.host} in Chatter?`,
      detail: current
        ? `This link is for a different server than the one you use (${new URL(current).host}). Switching signs you in there instead.`
        : "Chatter will connect to this server.",
    });
    if (response !== 0) return;
    setServerOrigin(page.origin);
  }
  void window.loadURL(page.toString());
}
