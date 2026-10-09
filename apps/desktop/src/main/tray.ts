import { Menu, Tray, nativeImage } from "electron";
import icon from "../../resources/icon.png?asset";

export interface TrayActions {
  show(): void;
  settings(): void;
  checkForUpdates(): void;
  changeServer(): void;
  quit(): void;
}

let tray: Tray | null = null;

export function createTray(actions: TrayActions): Tray {
  const image = nativeImage.createFromPath(icon).resize({ width: 32, height: 32, quality: "best" });
  tray = new Tray(image);
  tray.setToolTip("Chatter");
  tray.setContextMenu(
    Menu.buildFromTemplate([
      { label: "Open Chatter", click: actions.show },
      { label: "Settings…", click: actions.settings },
      { label: "Check for updates…", click: actions.checkForUpdates },
      { label: "Change server…", click: actions.changeServer },
      { type: "separator" },
      { label: "Quit Chatter", click: actions.quit },
    ]),
  );
  tray.on("click", actions.show);
  return tray;
}

export function setTrayUnread(count: number): void {
  tray?.setToolTip(count > 0 ? `Chatter — ${count} unread` : "Chatter");
}
