import { expect, test, type ElectronApplication, type Page } from "@playwright/test";
import { execFileSync, spawn } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loudestWindow, readStereoWav } from "./audio";
import { ensureLoggedIn, launchAt, quit, SERVER, storedUser } from "./chatter";

// Calls through Chatter's own UI in the desktop app, against a native test
// client in the same channel sending a 500 Hz tone and recording what it
// hears. One test per media stack:
//   - browser: Chromium's WebRTC, with its fake microphone (a beep);
//   - native:  chatter-engine, with a fake 660 Hz microphone and its speakers
//              recorded to a file.
//
// Needs: a local server (CHATTER_E2E_SERVER), the test accounts and room from
// `chatter-testclient register` / `setup-room` (CHATTER_E2E_ROOM), and built
// chatter-testclient and chatter-engine.
const ROOM = process.env["CHATTER_E2E_ROOM"];
const ROOM_NAME = process.env["CHATTER_E2E_ROOM_NAME"] ?? "Native voice spike";
const repoRoot = join(__dirname, "../../..");
const bin = (name: string) => join(repoRoot, "target/debug", process.platform === "win32" ? `${name}.exe` : name);

interface SlotHeard {
  peak_dbfs: number;
  user_id: string | null;
  wav: string;
}

function runNativeClient(seconds: number, out: string): Promise<{ slots_heard: Record<string, SlotHeard> }> {
  return new Promise((resolve, reject) => {
    const child = spawn(
      bin("chatter-testclient"),
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

async function joinCall(page: Page, nativeVoice: boolean): Promise<void> {
  await ensureLoggedIn(page, "browser_user");
  await page.evaluate((on) => localStorage.setItem("chatter_native_voice", on ? "on" : "off"), nativeVoice);
  await page.getByText(ROOM_NAME, { exact: true }).first().click();
  await page.getByText("General", { exact: true }).click();
}

function heardBrowserUser(report: { slots_heard: Record<string, SlotHeard> }): SlotHeard | undefined {
  return Object.values(report.slots_heard).find((s) => s.user_id === "@browser_user:localhost");
}

async function withApp(
  args: string[],
  env: Record<string, string>,
  body: (app: ElectronApplication, page: Page) => Promise<void>,
): Promise<void> {
  const { app } = await launchAt(args, "browser_user", env);
  const page = await app.firstWindow();
  try {
    await body(app, page);
  } catch (err) {
    await page.screenshot({ path: "test-results/voice-failure.png" }).catch(() => {});
    throw err;
  } finally {
    await quit(app);
  }
}

const ready = () => !SERVER || !ROOM || !storedUser("browser_user") || !storedUser("native_a");

test("browser voice: the desktop app's UI carries audio both ways", async () => {
  test.skip(ready(), "needs a local server and test accounts");
  test.setTimeout(120_000);
  await withApp(["--use-fake-device-for-media-stream"], {}, async (app, page) => {
    const native = runNativeClient(30, "recordings/e2e-browser");
    await page.waitForTimeout(2_000);
    await joinCall(page, false);
    await page.waitForTimeout(15_000);

    // The window audibly plays the native client: Chromium's own check.
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

    const report = await native;
    const heard = heardBrowserUser(report);
    expect(heard, JSON.stringify(report.slots_heard)).toBeTruthy();
    expect(heard!.peak_dbfs).toBeGreaterThan(-50);
  });
});

test("native voice: the engine carries the call both ways", async () => {
  test.skip(ready(), "needs a local server and test accounts");
  test.setTimeout(120_000);
  const speakers = join(mkdtempSync(join(tmpdir(), "chatter-engine-out-")), "speakers.wav");
  const env = {
    CHATTER_ENGINE_PATH: bin("chatter-engine"),
    CHATTER_ENGINE_FAKE_INPUT: "tone:660",
    CHATTER_ENGINE_FAKE_OUTPUT: speakers,
  };
  await withApp([], env, async (_app, page) => {
    const features = await page.evaluate(() => (window as unknown as { chatterDesktop?: { features: string[] } }).chatterDesktop?.features);
    expect(features).toContain("voice-backend@1");

    const native = runNativeClient(30, "recordings/e2e-native");
    await page.waitForTimeout(2_000);
    await joinCall(page, true);
    await page.waitForTimeout(15_000);

    // The engine's speakers played the native client's tone (tone-continuous.wav is 500 Hz).
    const { left, rate } = readStereoWav(speakers);
    const played = loudestWindow(left, rate);
    expect(played.rms, "engine output level").toBeGreaterThan(0.02);
    expect(Math.abs(played.hz - 500), `engine output pitch ${played.hz} Hz`).toBeLessThan(25);

    // The native client heard the engine's 660 Hz microphone.
    const report = await native;
    const heard = heardBrowserUser(report);
    expect(heard, JSON.stringify(report.slots_heard)).toBeTruthy();
    expect(heard!.peak_dbfs).toBeGreaterThan(-40);
  });
});

test("native voice: a call survives the engine crashing", async () => {
  test.skip(ready(), "needs a local server and test accounts");
  test.setTimeout(150_000);
  const speakers = join(mkdtempSync(join(tmpdir(), "chatter-engine-out-")), "speakers.wav");
  const env = {
    CHATTER_ENGINE_PATH: bin("chatter-engine"),
    CHATTER_ENGINE_FAKE_INPUT: "tone:660",
    CHATTER_ENGINE_FAKE_OUTPUT: speakers,
  };
  await withApp([], env, async (app, page) => {
    const native = runNativeClient(45, "recordings/e2e-crash");
    await page.waitForTimeout(2_000);
    await joinCall(page, true);
    await page.waitForTimeout(10_000);

    // Kill the engine outright. The app restarts it; the call's peers fail,
    // the client's retries rebuild them, and the mic reopens on the new one.
    const pid = await app.evaluate(() => {
      const { execSync } = process.mainModule!.require("node:child_process") as typeof import("node:child_process");
      const name = process.platform === "win32" ? "chatter-engine.exe" : "chatter-engine";
      return execSync(process.platform === "win32" ? `tasklist /FI "IMAGENAME eq ${name}" /FO CSV /NH` : `pgrep -x ${name}`).toString();
    });
    expect(pid).toContain(process.platform === "win32" ? "chatter-engine" : "");
    if (process.platform === "win32") execFileSync("taskkill", ["/IM", "chatter-engine.exe", "/F"]);
    else execFileSync("pkill", ["-x", "chatter-engine"]);
    await page.waitForTimeout(20_000);

    // The restarted engine writes a fresh speakers file: the other side is
    // audible again.
    const { left, rate } = readStereoWav(speakers);
    const played = loudestWindow(left, rate);
    expect(played.rms, "audio after the restart").toBeGreaterThan(0.02);

    // And the other side hears us again: the end of its recording of us.
    const report = await native;
    const heard = heardBrowserUser(report);
    expect(heard, JSON.stringify(report.slots_heard)).toBeTruthy();
    const ours = readStereoWav(join(repoRoot, heard!.wav));
    const tail = ours.left.slice(Math.max(0, ours.left.length - 4 * ours.rate));
    expect(loudestWindow(tail, ours.rate).rms, "heard after the restart").toBeGreaterThan(0.02);
  });
});
