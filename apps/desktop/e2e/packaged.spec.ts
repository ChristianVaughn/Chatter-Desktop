import { _electron as electron, test, expect } from "@playwright/test";
import { existsSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { quit } from "./chatter";
// The packaged app (`npm run dist`) finds its bundled engine and offers the
// native features. Skipped when there's no packaged build or no server.
const exe =
  process.platform === "win32"
    ? join(__dirname, "../dist/win-unpacked/Chatter.exe")
    : join(__dirname, "../dist/linux-unpacked/chatter-desktop");
const SERVER = process.env["CHATTER_E2E_SERVER"];

test("the packaged app runs its bundled engine", async () => {
  test.skip(!existsSync(exe) || !SERVER, "build with `npm run dist` and set CHATTER_E2E_SERVER");
  const profile = mkdtempSync(join(tmpdir(), "chatter-pkg-"));
  writeFileSync(join(profile, "config.json"), JSON.stringify({ serverOrigin: new URL(SERVER!).origin }));
  const env = { ...process.env, CHATTER_USER_DATA: profile };
  delete (env as Record<string, string | undefined>).CHATTER_ENGINE_PATH;
  const app = await electron.launch({ executablePath: exe, env });
  const page = await app.firstWindow();
  await page.waitForURL(`${new URL(SERVER!).origin}/`);
  const features = await page.evaluate(() => (window as unknown as { chatterDesktop: { features: string[] } }).chatterDesktop.features);
  expect(await app.evaluate(({ app }) => app.isPackaged)).toBe(true);
  // App audio needs Windows 11 / a sound server, so only the rest is required.
  expect(features).toEqual(expect.arrayContaining(["voice-backend@1", "ptt", "game-activity"]));
  await quit(app);
});
