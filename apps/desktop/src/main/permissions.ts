import type { Session } from "electron";
import { isServerUrl } from "./origin";

// What the Chatter client actually uses. Everything else is refused, and
// nothing is granted to any origin but the user's own server.
const ALLOWED = new Set<string>([
  "media",
  "display-capture",
  "notifications",
  "clipboard-read",
  "clipboard-sanitized-write",
  "fullscreen",
  "pointerLock",
]);

export function installPermissionHandlers(session: Session): void {
  session.setPermissionRequestHandler((_contents, permission, callback, details) => {
    callback(ALLOWED.has(permission) && isServerUrl(details.requestingUrl));
  });
  session.setPermissionCheckHandler((_contents, permission, requestingOrigin) => {
    return ALLOWED.has(permission) && isServerUrl(requestingOrigin);
  });
}
