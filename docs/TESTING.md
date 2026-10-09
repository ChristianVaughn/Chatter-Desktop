# Testing

## What runs where

| Suite | Command | Needs |
|---|---|---|
| Rust unit tests: resampling, spatial gains, speech detection, SDP munging, hotkey tables, audio chunking, ducking ramps | `cargo test --workspace` | — |
| Hardware tests: injected key presses, capture of a child process's tone, ducking a real session | `cargo test -p chatter-hotkeys -p chatter-appaudio -- --ignored --test-threads=1` | Windows with an audio device |
| Engine over its stdio protocol with fake devices: hello, devices, mic levels, mute, mic test and monitor, push-to-talk, reset, clean shutdown | `npx playwright test e2e/engine.spec.ts` (in `apps/desktop`) | built engine |
| Shell: server picker, quitting stops the engine cleanly, bad address, bridge exposure, external links, deep links | `npx playwright test e2e/smoke.spec.ts` | `npm run build`; server for the last three |
| Voice through Chatter's UI | `npx playwright test e2e/voice.spec.ts` | server, test accounts, built engine and testclient |
| Screen share with app audio | `npx playwright test e2e/screenshare.spec.ts` | Windows 11, server |
| Packaged build | `npx playwright test e2e/packaged.spec.ts` | `npm run dist`, server |
| Chatter client (in the Chatter repo) | `cd client && npx vitest run` | — |

CI (`.github/workflows/ci.yml`) runs the first, third and fourth rows (the shell tests without a server), plus `cargo fmt`, clippy, the contract check and packaging, on Windows and Ubuntu.

## The voice tests

`e2e/voice.spec.ts` drives Chatter's real UI in the desktop app against `chatter-testclient`, a headless native client in the same channel that plays a 500 Hz tone and records every slot.

| Test | Proves |
|---|---|
| **browser voice** | Chromium's WebRTC in the app is audible (Chromium's own audibility check on the window) and is heard by the other side. |
| **native voice** | The engine's fake 660 Hz mic is heard by the other side. The engine's recorded speakers contain the other side's 500 Hz tone. Turning "Lower other apps" up ducks other apps while the other side talks and restores them after. |
| **engine crash** | The engine is killed mid-call. The app restarts it, the client's retries rebuild both connections, and audio flows both ways again. |

### Setup, once per server

```bash
cargo build -p chatter-testclient -p chatter-engine
```

```bash
cargo run -p chatter-testclient -- register spike_admin
cargo run -p chatter-testclient -- register native_a
cargo run -p chatter-testclient -- register native_b
cargo run -p chatter-testclient -- register browser_user
```

```bash
cargo run -p chatter-testclient -- setup-room spike_admin native_a native_b browser_user
```

`setup-room` prints the room id. Then:

```bash
CHATTER_E2E_SERVER=http://localhost:8000 CHATTER_E2E_ROOM='<room id>' npx playwright test
```

**Notes**
- The test accounts (passwords and TOTP secrets) live in `.testclient/users.json`, which is gitignored.
- The server allows 5 sign-ups an hour per address and 10 logins per 5 minutes per user. The voice tests keep a signed-in profile under `test-results/profiles/` so they rarely need to log in.
- The native client plays `recordings/tone-continuous.wav`, a 500 Hz tone the tests write when it is missing.

## Bugs these tests found

- **Server: WebRTC dies on Windows hosts.** See SPIKE-RESULTS.md.
- **LiveKit bindings: default offer options turn receiving off.** See SPIKE-RESULTS.md.
- **Server: closing a spare tab cut an ongoing call's audio.** Reproduced with `scripts/spare-tab-test.sh`.
- **Client: calls were silent on Chromium 152.** Remote audio routed through Web Audio is silent while an element also plays the track. Fixed by giving Web Audio a clone of the track.
- **Desktop:** a publisher retried while the engine was restarting failed for good. Requests now wait for the restart, and a failed engine operation marks the peer failed so the client retries.

## Existing failures in Chatter's own tests

`cargo test --test ws_contract` in the Chatter repo fails on `main` as well as on `desktop`. Its helpers register with a 2-character password and expect a token back, which the current two-step TOTP registration doesn't do, and they share the dev server's `chatter` database.
