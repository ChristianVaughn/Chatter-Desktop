import type { Session } from "electron";

/**
 * Keeps the sign-in on disk. Chatter's refresh cookie is single-use: every
 * refresh deletes the old token on the server and sets a new cookie. Chromium
 * writes cookie changes to disk only about every 30 s, so an app that dies in
 * between (a crash, a shutdown, or the update installer, which kills it about
 * 1.3 s after starting) comes back with a token the server no longer has, and
 * is signed out. So write cookie changes as they happen.
 */
export function persistCookiesPromptly(target: Session): void {
  let pending = false;
  target.cookies.on("changed", () => {
    if (pending) return;
    pending = true;
    // A refresh sets more than one cookie; one write covers them.
    setTimeout(() => {
      pending = false;
      target.cookies.flushStore().catch(() => {});
    }, 100);
  });
}

/** Writes everything the page keeps (cookies, localStorage) to disk now. */
export async function flushSession(target: Session): Promise<void> {
  target.flushStorageData();
  await target.cookies.flushStore().catch(() => {});
}
