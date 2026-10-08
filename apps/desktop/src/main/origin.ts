import { getServerOrigin } from "./serverStore";

/** Whether a URL belongs to the Chatter server the user picked. */
export function isServerUrl(raw: string | undefined | null): boolean {
  const server = getServerOrigin();
  if (!server || !raw) return false;
  try {
    return new URL(raw).origin === server;
  } catch {
    return false;
  }
}

/**
 * Third-party pages the main window may navigate to in place, for sign-in
 * round-trips that must come back to the server in the same window.
 */
const SIGN_IN_ORIGINS = new Set(["https://steamcommunity.com"]);

export function isSignInUrl(raw: string): boolean {
  try {
    return SIGN_IN_ORIGINS.has(new URL(raw).origin);
  } catch {
    return false;
  }
}

export function isWebUrl(raw: string): boolean {
  try {
    const { protocol } = new URL(raw);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}
