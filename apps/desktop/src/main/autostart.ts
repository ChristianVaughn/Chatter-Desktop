import { app } from "electron";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

/** Passed when the system starts us, so we can stay in the tray. */
export const HIDDEN_FLAG = "--hidden";

export function launchedAtLogin(): boolean {
  return process.argv.includes(HIDDEN_FLAG) || app.getLoginItemSettings().wasOpenedAtLogin === true;
}

/** Linux has no login-item API; desktops honour ~/.config/autostart. */
function linuxAutostartFile(): string {
  const config = process.env["XDG_CONFIG_HOME"] || join(homedir(), ".config");
  return join(config, "autostart", "chatter-desktop.desktop");
}

export function setStartWithSystem(enabled: boolean): void {
  // A development build would register electron.exe itself; don't.
  if (!app.isPackaged) return;
  if (process.platform === "linux") {
    const file = linuxAutostartFile();
    if (!enabled) {
      rmSync(file, { force: true });
      return;
    }
    // An AppImage runs from a temporary mount; $APPIMAGE is the file itself.
    const exe = process.env["APPIMAGE"] || process.execPath;
    mkdirSync(join(file, ".."), { recursive: true });
    writeFileSync(
      file,
      [
        "[Desktop Entry]",
        "Type=Application",
        "Name=Chatter",
        `Exec="${exe}" ${HIDDEN_FLAG}`,
        "X-GNOME-Autostart-enabled=true",
        "",
      ].join("\n"),
    );
    return;
  }
  app.setLoginItemSettings({ openAtLogin: enabled, args: [HIDDEN_FLAG] });
}
