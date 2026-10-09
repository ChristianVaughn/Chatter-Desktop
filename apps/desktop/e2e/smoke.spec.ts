import { _electron as electron, expect, test, type ElectronApplication } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { quit } from "./chatter";

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
  await quit(app);
  rmSync(profile, { recursive: true, force: true, maxRetries: 5 });
});

test("first launch asks for a server", async () => {
  const page = await app.firstWindow();
  await expect(page.locator("h1")).toHaveText("Connect to a Chatter server");
  // The shell API exists here and nowhere else.
  expect(await page.evaluate(() => "shellApi" in window)).toBe(true);
  expect(await page.evaluate(() => "chatterDesktop" in window)).toBe(false);
});

test("quitting stops the engine cleanly", async () => {
  const log = join(profile, "logs", "engine.log");
  await app.firstWindow();
  test.skip(!existsSync(log), "build chatter-engine first");
  await quit(app);
  // Asked to stop, not killed: it got to put things back first.
  const text = readFileSync(log, "utf8");
  expect(text.slice(-2000)).toContain("chatter-engine stopped");
});

test("a cookie survives the app being killed a second after it changed", async () => {
  // The update installer kills the app about 1.3 s after it starts, and
  // Chatter's refresh cookie is single-use: one not on disk by then is a
  // sign-out. A second session without prompt writes is the control.
  await app.firstWindow();
  const expirationDate = Date.now() / 1000 + 3600;
  await app.evaluate(async ({ session }, expirationDate) => {
    const cookie = { url: "http://localhost/", name: "refresh_token", value: "rotated", expirationDate };
    await session.defaultSession.cookies.set(cookie);
    await session.fromPartition("persist:control").cookies.set(cookie);
  }, expirationDate);
  await new Promise((r) => setTimeout(r, 1000));
  killTree(app.process().pid!);
  await new Promise((r) => setTimeout(r, 1000));

  app = await electron.launch({
    args: [join(__dirname, "..")],
    env: { ...process.env, CHATTER_USER_DATA: profile },
  });
  await app.firstWindow();
  const [kept, control] = await app.evaluate(async ({ session }) => {
    const find = (s: Electron.Session) => s.cookies.get({ name: "refresh_token" }).then((c) => c[0]?.value ?? null);
    return [await find(session.defaultSession), await find(session.fromPartition("persist:control"))];
  });
  expect(kept).toBe("rotated");
  // If Chromium ever writes promptly by itself, the control shows it.
  test.info().annotations.push({ type: "control", description: `without prompt writes: ${control ?? "lost"}` });
});

/** Kills the app and its helpers without letting them shut down, as the installer does. */
function killTree(pid: number): void {
  if (process.platform === "win32") execFileSync("taskkill", ["/F", "/T", "/PID", String(pid)], { stdio: "ignore" });
  else process.kill(pid, "SIGKILL");
}

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
  expect(bridge).toMatchObject({ bridgeVersion: 1 });
  // With chatter-engine built, the native features are on offer.
  expect((bridge as { features: string[] }).features).toEqual(expect.arrayContaining(["voice-backend@1", "ptt"]));
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

test("a chatter:// link opens that page of the server", async () => {
  test.skip(!SERVER, "set CHATTER_E2E_SERVER to run");
  // A fresh app, already pointed at the server, launched by the link.
  await quit(app);
  const { launchAt } = await import("./chatter");
  const page = `${new URL(SERVER!).origin}/?from=deeplink`;
  const launched = await launchAt([`chatter://open?url=${encodeURIComponent(page)}`]);
  app = launched.app;
  profile = launched.profile;
  const window = await app.firstWindow();
  await window.waitForURL(page);
});
