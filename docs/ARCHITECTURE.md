# Chatter Desktop: architecture

This document is for the Chatter maintainers and their agents. Supporting documents:
- [SPIKE-RESULTS.md](SPIKE-RESULTS.md): the native voice proof;
- [ENGINE.md](ENGINE.md): the engine and its protocol;
- [TESTING.md](TESTING.md): how it's tested;
- [UPSTREAM-PR.md](UPSTREAM-PR.md): the changes it needs in Chatter.

## Goal

A proper desktop app for Chatter, comparable to Discord desktop:
- OS integration: tray, native notifications, badge, deep links, auto-update;
- a **native voice engine**: better noise suppression, global push-to-talk, output devices, ducking;
- **screen share with per-app audio**;
- game activity.

Targets: Windows and Linux. macOS is possible later.

## Decisions

### Electron shell, not Tauri

- **Tauri on Linux:** Tauri renders with WebKitGTK. Distro builds of WebKitGTK compile out `RTCPeerConnection`, and its Chromium (CEF) runtime is still alpha.
- **Video into the UI:** pushing natively decoded video into a webview isn't viable at 1080p60.
- **What Electron gives us:**
  - Chromium keeps *receiving* screen shares and webcams with hardware decode, so that path needs no client changes.
  - Native code takes over voice and capture, where it adds value.

### The shell loads the server, as Discord loads discord.com/app

The client assumes it is same-origin with its server:
- about 200 relative `fetch("/api/…")` calls;
- a WebSocket built from `location.host`;
- an `HttpOnly; SameSite=Strict` refresh cookie;
- CORS only on `/external`.

Loading the user's server URL keeps all of that working unchanged. The UI updates with every server deploy and can't drift from the server. There is no shared-UI repository.

### A narrow, capability-negotiated bridge

- **What the preload exposes.** `window.chatterDesktop = { bridgeVersion, appVersion, platform, features[], … }`, and only to the configured server's origin. The rest of the object depends on the features the app offers: `voiceBackend`, `pushToTalk`, `displayCapture`, `gameActivity` and `ducking`. The shell's own pages get a separate `shellApi` instead. The main process decides which a page gets from the frame URL, not from anything the page says.
- **Version skew.** The client comes from the server but the shell is installed, so they drift apart. The client checks `features.includes("…")` before using anything, and features are only ever added, never renamed.
- **Security.** The server is user-chosen, so the bridge never exposes:
  - files or shell access;
  - raw keystrokes;
  - process lists;
  - a way to start a capture.

  Pickers, keybind capture and settings are windows the shell owns. The page reaches the engine only through an allowlist of call operations (see ENGINE.md).

### The native engine is a separate Rust executable (`chatter-engine`)

- **Stack.** Google libwebrtc via LiveKit's `libwebrtc` bindings, used standalone with no LiveKit server. Zed ships on this stack.
- **Why its own process:**
  - libwebrtc's BoringSSL can't clash with Electron's Chromium;
  - there are no Node ABI rebuilds;
  - it can be tested headless;
  - a crash doesn't take the window down.

### Chatter's signalling stays in TypeScript

`useWebRTCVoice` holds all the Chatter-specific behaviour: retries, `voice_force_muted`, `voice_session_taken`, SDP munging. The maintainer's agents change it often.

The engine exposes only what a browser does:
- an `RTCPeerConnection`-shaped **`VoicePeer`**;
- a **`VoiceMediaBackend`** for the mic, the playout mixer, per-user gain and spatial pan, devices, mic test and push-to-talk.

The renderer keeps the single chat WebSocket. The server routes signalling by user and accepts leave/move only from the session-holding socket. Offers, answers and candidates are relayed to the engine through the renderer, so the engine never opens a socket of its own.

The page's `VoicePeer` and `VoiceMediaBackend` are built in its own JavaScript world (`preload/mainWorld.ts`), so the objects the client assigns handlers to are real page objects. They talk to the engine through `window.__chatterEngine`, which goes page → preload → main process (origin and allowlist checks) → engine over stdio.

```
Chatter-Desktop
├─ apps/desktop
│   ├─ src/main        window, server picker, nav lock, permissions, tray, badge,
│   │                  notifications, capture picker + app audio, engine host and
│   │                  bridge, settings, game detection, updater, deep links, autostart
│   ├─ src/preload     one preload; role decided by the main process. mainWorld.ts is
│   │                  the page-world bridge and voice backend
│   ├─ src/shell-ui    server picker, capture picker, push-to-talk key capture,
│   │                  settings, offline page
│   ├─ src/contract    the client's bridge and media types (scripts/sync-contract.mjs)
│   └─ e2e             Playwright: shell, engine protocol, voice, screen share, packaging
├─ crates/chatter-engine    the media process (ENGINE.md)
├─ crates/chatter-media     facade over libwebrtc
├─ crates/chatter-hotkeys   system-wide push-to-talk
├─ crates/chatter-appaudio  per-app capture and ducking
└─ crates/chatter-testclient  headless Chatter voice client for tests

Chatter (fork, branch `desktop`), additive
├─ client/src/lib/desktop/bridge.ts   window.chatterDesktop types (source of truth)
├─ client/src/lib/media/              VoiceMediaBackend seam, browser implementation
└─ server                             desktop connections, push, game activity, fixes
```

## Status

Every phase below is built and tested. See TESTING.md for what each test proves.

| Phase | What | State |
|---|---|---|
| 0 | Spike: native voice against the real SFU | ✅ (SPIKE-RESULTS.md) |
| 1 | Shell: server picker, offline page, nav lock, permissions, tray, single instance, native toasts, unread badge, capture picker, context menu, packaging | ✅ |
| 2 | Chatter changes (fork, `desktop` branch). Server: Windows WebRTC fix, spare-tab fix, desktop-aware push. Client: bridge, media seam, voice settings applied live, Chrome 152 silent-call fix | ✅ |
| 3 | Native voice: engine, mic pipeline (echo cancellation, noise suppression, RNNoise), mixer with spatial audio, devices, mic test, system-wide push-to-talk, crash recovery | ✅ |
| 4 | Screen share with the shared app's audio (Windows 11+, PulseAudio/PipeWire) | ✅ |
| 5 | Ducking, game activity, settings window, deep links, start with system, auto-update, engine log | ✅ |

**Not verified here:**
- **Linux runtime.** It compiles and passes clippy, and the unit tests run in CI. Push-to-talk on X11 and Wayland, PulseAudio capture and ducking haven't been run on a Linux machine.
- **macOS.** Not built. The engine's audio and peers would work, but push-to-talk and app audio have no macOS backend.
- **Code signing.** Configuration is documented in RELEASING.md; no certificate has been used.

## Risks

- **Echo for speaker users.** The engine's echo canceller sees the call's mix, not screen-share audio played by Chromium. Recommend headphones. A system loopback as an extra reference is a possible follow-up.
- **Wayland push-to-talk.** Portal shortcuts are keys only, depend on the desktop environment, and may be toggle-only.
- **Windows 10.** It has no process loopback before build 20348, so the picker offers no audio there.
- **Antivirus heuristics.** Unsigned executables with keyboard hooks and audio capture look suspicious. Sign both executables.
- **Churn in LiveKit's bindings.** Mitigation: pin the version exactly and keep all binding code in `chatter-media`.
- **Ducking that outlives the engine.** Windows and PulseAudio/WirePlumber remember each app's volume, even across a reboot, so a volume left lowered comes back every time that app plays. The engine restores before exiting, including when the OS ends the session (`session_end.rs`). Its journal covers a hard kill or power loss: on the next start it puts back each app that comes back at the volume it was left at. A machine that never starts Chatter again keeps the lowered volumes until the user resets them in the volume mixer.
