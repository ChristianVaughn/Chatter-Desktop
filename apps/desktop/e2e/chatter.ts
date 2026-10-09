// Helpers for driving a real Chatter server through the desktop app.

import { _electron as electron, expect, type ElectronApplication, type Page } from "@playwright/test";
import { createHmac } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const SERVER = process.env["CHATTER_E2E_SERVER"];
/** Test accounts created by `chatter-testclient register` (gitignored). */
const USERS_FILE = join(__dirname, "../../../.testclient/users.json");

interface StoredUser {
  password: string;
  totp_secret: string;
}

export function storedUser(username: string): StoredUser | null {
  if (!SERVER) return null;
  try {
    const all = JSON.parse(readFileSync(USERS_FILE, "utf8")) as { servers: Record<string, Record<string, StoredUser>> };
    return all.servers[new URL(SERVER).origin]?.[username] ?? null;
  } catch {
    return null;
  }
}

function base32(secret: string): Buffer {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = 0;
  let value = 0;
  const out: number[] = [];
  for (const c of secret.replace(/=+$/, "").toUpperCase()) {
    value = (value << 5) | alphabet.indexOf(c);
    bits += 5;
    if (bits >= 8) {
      out.push((value >>> (bits - 8)) & 255);
      bits -= 8;
    }
  }
  return Buffer.from(out);
}

/** RFC 6238, as the server checks it: SHA1, 6 digits, 30 s. */
export function totp(secret: string): string {
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(Math.floor(Date.now() / 30_000)));
  const h = createHmac("sha1", base32(secret)).update(counter).digest();
  const o = h[h.length - 1] & 0xf;
  return String((h.readUInt32BE(o) & 0x7fffffff) % 1_000_000).padStart(6, "0");
}

/**
 * Launch the built app pointed at SERVER. A named profile persists between
 * runs (under test-results/, ignored), so a signed-in session is reused rather
 * than logging in each time: the server allows few logins per user.
 */
export async function launchAt(
  extraArgs: string[] = [],
  persistentProfile?: string,
  env: Record<string, string> = {},
): Promise<{ app: ElectronApplication; profile: string }> {
  const profile = persistentProfile
    ? join(__dirname, "../test-results/profiles", persistentProfile)
    : mkdtempSync(join(tmpdir(), "chatter-e2e-"));
  mkdirSync(profile, { recursive: true });
  writeFileSync(join(profile, "config.json"), JSON.stringify({ serverOrigin: new URL(SERVER!).origin }));
  const app = await electron.launch({
    args: [join(__dirname, ".."), ...extraArgs],
    env: { ...process.env, ...env, CHATTER_USER_DATA: profile },
  });
  return { app, profile };
}

/** Sign in, unless the profile's saved session already did. */
export async function ensureLoggedIn(page: Page, username: string): Promise<void> {
  const signedIn = page.getByText("Logout", { exact: true });
  const form = page.locator("#username");
  await expect(signedIn.or(form).first()).toBeVisible({ timeout: 30_000 });
  if (await signedIn.isVisible()) return;
  await login(page, username);
}

export async function login(page: Page, username: string): Promise<void> {
  const user = storedUser(username);
  if (!user) throw new Error(`no stored test account ${username}`);
  await page.locator("#username").fill(username);
  await page.locator("#password").fill(user.password);
  await page.locator("button[type=submit]").click();
  const code = page.locator("#totp");
  await code.waitFor({ timeout: 15_000 });
  await code.fill(totp(user.totp_secret));
  await page.locator("button[type=submit]").click();
  // The form disappears into a "connecting" animation before the response is
  // back, so wait for the signed-in app itself.
  await expect(page.getByText("Logout", { exact: true })).toBeVisible({ timeout: 30_000 });
}

/** Quit for real (closing only hides to the tray) and wait for the process. */
export async function quit(app: ElectronApplication): Promise<void> {
  const exited = new Promise<void>((resolve) => app.process().once("exit", () => resolve()));
  await app.evaluate(({ app }) => app.quit()).catch(() => {});
  await exited;
}
