import { expect, test } from "@playwright/test";
import { spawn } from "node:child_process";
import { join } from "node:path";
import { ensureLoggedIn, launchAt, quit, SERVER, storedUser } from "./chatter";

// A voice call through the real Chatter UI in the desktop app, with Chromium's
// fake microphone (a beep), against a native client in the same channel.
//
// Needs: a local server (CHATTER_E2E_SERVER), the test accounts and room from
// `chatter-testclient register` / `setup-room` (CHATTER_E2E_ROOM), and a built
// chatter-testclient.
const ROOM = process.env["CHATTER_E2E_ROOM"];
const ROOM_NAME = process.env["CHATTER_E2E_ROOM_NAME"] ?? "Native voice spike";
const repoRoot = join(__dirname, "../../..");
const testclient = join(repoRoot, "target/debug", process.platform === "win32" ? "chatter-testclient.exe" : "chatter-testclient");

interface SlotHeard {
  peak_dbfs: number;
  user_id: string | null;
}

function runNativeClient(seconds: number, out: string): Promise<{ slots_heard: Record<string, SlotHeard> }> {
  return new Promise((resolve, reject) => {
    const child = spawn(
      testclient,
      ["--server", SERVER!, "voice", "native_a", "--room", ROOM!, "--wav", "recordings/tone-continuous.wav", "--seconds", String(seconds), "--out", out],
      { cwd: repoRoot },
    );
    let stdout = "";
    child.stdout.on("data", (d) => (stdout += d));
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code !== 0) return reject(new Error(`native client exited ${code}`));
      resolve(JSON.parse(stdout.slice(stdout.indexOf("{"))));
    });
  });
}

test("a call through the desktop app's UI carries audio both ways with a native client", async () => {
  test.skip(!SERVER || !ROOM || !storedUser("browser_user") || !storedUser("native_a"), "needs a local server and test accounts");
  test.setTimeout(120_000);

  const { app } = await launchAt(["--use-fake-device-for-media-stream"], "browser_user");
  try {
    const page = await app.firstWindow();
    await ensureLoggedIn(page, "browser_user");
    await page.getByText(ROOM_NAME, { exact: true }).first().click();

    const native = runNativeClient(30, "recordings/e2e-native");
    await page.waitForTimeout(2_000);
    await page.getByText("General", { exact: true }).click();
    await page.waitForTimeout(15_000);

    // The page is actually playing the native client's tone: Chromium's own
    // audibility check on the window's output, polled for a few seconds.
    const audible = await app.evaluate(async ({ BrowserWindow }) => {
      const contents = BrowserWindow.getAllWindows()[0].webContents;
      let hits = 0;
      for (let i = 0; i < 20; i++) {
        if (contents.isCurrentlyAudible()) hits++;
        await new Promise((r) => setTimeout(r, 250));
      }
      return hits;
    });
    expect(audible, "the window plays the native client's audio").toBeGreaterThan(10);

    // And the native client heard the browser user's fake microphone.
    const report = await native;
    const heard = Object.values(report.slots_heard).find((s) => s.user_id === "@browser_user:localhost");
    expect(heard, JSON.stringify(report.slots_heard)).toBeTruthy();
    expect(heard!.peak_dbfs).toBeGreaterThan(-50);
  } catch (err) {
    await (await app.firstWindow()).screenshot({ path: "test-results/voice-failure.png" }).catch(() => {});
    throw err;
  } finally {
    await quit(app);
  }
});
