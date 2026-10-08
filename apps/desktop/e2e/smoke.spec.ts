import { _electron as electron, expect, test, type ElectronApplication } from "@playwright/test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Runs against the built app (`npm run build` first). Tests that need a live
// Chatter server read its address from CHATTER_E2E_SERVER and are skipped
// without it, so CI can run the rest with no server.
const SERVER = process.env["CHATTER_E2E_SERVER"];

let app: ElectronApplication;
let profile: string;

test.beforeEach(async () => {
  profile = mkdtempSync(join(tmpdir(), "chatter-e2e-"));
  app = await electron.launch({
    args: [join(__dirname, "..")],
    env: { ...process.env, CHATTER_USER_DATA: profile },
  });
});

test.afterEach(async () => {
  // Closing the window only hides it to the tray; quit for real.
  await app.evaluate(({ app }) => app.quit());
  await app.close().catch(() => {});
  rmSync(profile, { recursive: true, force: true });
});

test("first launch asks for a server", async () => {
  const page = await app.firstWindow();
  await expect(page.locator("h1")).toHaveText("Connect to a Chatter server");
  // The shell API exists here and nowhere else.
  expect(await page.evaluate(() => "shellApi" in window)).toBe(true);
  expect(await page.evaluate(() => "chatterDesktop" in window)).toBe(false);
});

test("rejects an address that isn't a Chatter server", async () => {
  const page = await app.firstWindow();
  await page.fill("#url", "127.0.0.1:9");
  await page.click("#submit");
  await expect(page.locator("#error")).toContainText("Couldn't reach the server");
});

test("connects to a server and exposes the bridge", async () => {
  test.skip(!SERVER, "set CHATTER_E2E_SERVER to run");
  const page = await app.firstWindow();
  await page.fill("#url", SERVER!);
  await page.click("#submit");
  await page.waitForURL(`${new URL(SERVER!).origin}/`);

  const bridge = await page.evaluate(() => (window as unknown as { chatterDesktop?: unknown }).chatterDesktop);
  expect(bridge).toMatchObject({ bridgeVersion: 1, features: [] });
  expect(await page.evaluate(() => "shellApi" in window)).toBe(false);
  // Notification clicks restore the window from the tray.
  expect(await page.evaluate(() => Notification.name)).toBe("DesktopNotification");
  expect(await page.evaluate(() => typeof Notification.requestPermission)).toBe("function");

  // The choice survives a restart.
  const saved = JSON.parse(readFileSync(join(profile, "config.json"), "utf8"));
  expect(saved.serverOrigin).toBe(new URL(SERVER!).origin);
});

test("external links leave the app", async () => {
  test.skip(!SERVER, "set CHATTER_E2E_SERVER to run");
  const page = await app.firstWindow();
  await page.fill("#url", SERVER!);
  await page.click("#submit");
  await page.waitForURL(`${new URL(SERVER!).origin}/`);

  // Record openExternal instead of launching a browser.
  await app.evaluate(({ shell }) => {
    (globalThis as unknown as { opened: string[] }).opened = [];
    shell.openExternal = async (url: string) => {
      (globalThis as unknown as { opened: string[] }).opened.push(url);
    };
  });
  await page.evaluate(() => {
    window.location.href = "https://example.com/";
  });
  await expect
    .poll(() => app.evaluate(() => (globalThis as unknown as { opened: string[] }).opened))
    .toEqual(["https://example.com/"]);
  expect(new URL(page.url()).origin).toBe(new URL(SERVER!).origin);
});
