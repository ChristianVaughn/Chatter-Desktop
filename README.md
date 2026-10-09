# Chatter Desktop

The desktop app for [Chatter](https://github.com/Sphyrna-029/Chatter), the self-hosted chat app, for Windows and Linux.

It opens your Chatter server the way Discord's app opens discord.com, so the interface is always the one your server serves, and adds what a browser tab can't:

- **Native voice engine**: Google's libwebrtc in a separate Rust process, the same engine Chrome and Discord use. It adds enhanced noise suppression (RNNoise), echo cancellation, a choice of output device, and calls that survive the engine crashing.
- **System-wide push-to-talk**: works while a game has focus. Any key or a side mouse button.
- **Screen share with the app's own audio**: share a game with just its sound, or everything except Chatter, so the call isn't sent back to itself.
- **Lower other apps while people talk** (Discord's "attenuation").
- **"Playing …" status** from the game you have open.
- **Tray, notifications and badge**: closes to the tray, keeps notifying, shows the unread count on the taskbar.
- **The rest**: `chatter://` links, start with the system, automatic updates.

In a browser, Chatter keeps working exactly as before. Each desktop feature switches on only when the desktop app says it supports it.

## How it fits together

```
Chatter server ──serves──▶ Chatter client (React) ── running inside ──▶ Electron window
                                   │  window.chatterDesktop (preload bridge)
                                   ▼
                     Electron main process ──stdio──▶ chatter-engine (Rust)
                     tray, picker, settings,           libwebrtc voice, mic/speaker pipeline,
                     updater, deep links               push-to-talk, app audio, ducking
```

The read-in-order documents:
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): the design and why.
- [docs/ENGINE.md](docs/ENGINE.md): the engine and its protocol.
- [docs/TESTING.md](docs/TESTING.md): how it's tested.
- [docs/RELEASING.md](docs/RELEASING.md): packaging, signing, updates.
- [docs/SPIKE-RESULTS.md](docs/SPIKE-RESULTS.md): the native voice proof.

The changes it needs in Chatter itself are on the `desktop` branch of the Chatter fork; [docs/UPSTREAM-PR.md](docs/UPSTREAM-PR.md) describes them.

## Layout

| Path | What |
|---|---|
| `apps/desktop` | Electron app (TypeScript, electron-vite, electron-builder) |
| `apps/desktop/src/main` | Window, tray, permissions, picker, settings, engine host, updater, deep links |
| `apps/desktop/src/preload` | The bridge; `mainWorld.ts` is the voice backend the page uses |
| `apps/desktop/src/contract` | The client's bridge and media types, synced by `scripts/sync-contract.mjs` |
| `crates/chatter-engine` | The native media process |
| `crates/chatter-media` | Facade over libwebrtc |
| `crates/chatter-hotkeys` | System-wide push-to-talk |
| `crates/chatter-appaudio` | Per-app audio capture and ducking |
| `crates/chatter-testclient` | Headless Chatter voice client, used by the tests |

## Develop

**Requirements**
- Node 22, and Rust (version pinned in `rust-toolchain.toml`).
- Windows: VS 2022 Build Tools with the C++ workload.
- Linux: clang 21 or newer, plus `libglib2.0-dev`, `libasound2-dev`, `libpulse-dev`, `libx11-dev`, `libxi-dev`, `libdbus-1-dev`, `pkg-config`.

**Local server.** A Chatter checkout next to this one, plus MongoDB:

```bash
node scripts/run-chatter-server.mjs
```

**Engine** (once, and after Rust changes):

```bash
cargo build -p chatter-engine
```

**App** (hot reload). Point it at `localhost:8000` on first launch:

```bash
npm install
npm run dev
```

**Installer.** Builds the release engine, then an NSIS installer on Windows, or AppImage + deb on Linux:

```bash
npm run dist
```

## License

GPL-3.0-or-later, same as Chatter.
