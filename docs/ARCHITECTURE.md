# Chatter Desktop: architecture and roadmap

This document is for the Chatter maintainers, and for their agents, to review before any upstream change. The supporting evidence is in [SPIKE-RESULTS.md](SPIKE-RESULTS.md).

## Goal

A proper desktop app for Chatter, comparable to Discord desktop:
- OS integration: tray, native notifications, badge, deep links, auto-update;
- a **native voice engine**: better noise suppression, global push-to-talk, output devices, ducking;
- **screen share with per-app audio**;
- later, game activity.

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

- **What the preload exposes.** `window.chatterDesktop = { bridgeVersion, appVersion, platform, features[] }`, and only to the configured server's origin. The shell's own pages get a separate `shellApi` instead. The main process decides which a page gets from the frame URL, not from anything the page says.
- **Version skew.** The client comes from the server but the shell is installed, so they drift apart. The client checks `features.includes("…")` before using anything, and features are only ever added, never renamed.
- **Security.** The server is user-chosen, so the bridge never exposes:
  - files or shell access;
  - raw keystrokes;
  - process lists.

  Pickers and keybind capture are windows the shell owns.

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

```
Chatter-Desktop
├─ apps/desktop            Electron shell (TypeScript, electron-vite, electron-builder)
│   ├─ src/main            window, server picker, nav lock, permissions, tray, badge,
│   │                      notifications, screen-capture picker, context menu
│   ├─ src/preload         one preload; role decided by the main process
│   ├─ src/shell-ui        server picker, capture picker, offline page
│   └─ e2e                 Playwright (_electron) smoke tests
├─ crates/chatter-media    facade over libwebrtc (all binding-specific code lives here)
├─ crates/chatter-testclient  headless Chatter voice client (the spike; becomes an integration test driver)
└─ crates/chatter-engine   (Phase 3) the engine process

Chatter (upstream), additive changes only
└─ client/src/lib/desktop/bridge.ts   types for window.chatterDesktop (source of truth)
└─ client/src/lib/media/types.ts      VoicePeer / VoiceMediaBackend (Phase 2)
```

## Status

| Piece | State |
|---|---|
| Shell: server picker, offline page, nav lock, permissions, tray and close-to-tray, single instance, native toasts with tray restore on click, unread badge (Windows overlay, Linux count), capture picker, context menu with spelling fixes, NSIS/AppImage/deb config | ✅ built; 4 Playwright tests pass against a local server |
| Native voice spike | ✅ go. See SPIKE-RESULTS.md |
| Windows server bug (UDP mux dies) | ✅ fixed on fork branch `fix/windows-webrtc-udp-connreset`; ready to PR upstream |
| CI (Windows and Ubuntu: typecheck, build, package, e2e, cargo fmt/clippy/test) | written; first run happens on push |

## Roadmap

### Phase 2: upstream PRs to Chatter, small and in order

- **PR-0 (ready):** the Windows UDP fix.
- **PR-A (server):**
  - **Fix `cleanup_disconnect`** (`src/backend/ws/session.rs:2561`). It tears down the user's voice and screen media when *any* of their connections closes, before checking `holds_voice_session`. So closing a spare tab cuts the audio of a call running elsewhere. This bug exists today.
  - **Add a `client: {kind, version}` field** to the first WebSocket frame.
  - **Make push suppression desktop- and idle-aware** (`src/backend/push.rs:327`). Today any open socket silences phone pushes, and a tray app keeps one open all day.
- **PR-B:**
  - `lib/desktop/bridge.ts`;
  - push-to-talk through the bridge;
  - Web Push reported as unsupported in desktop (Electron exposes `PushManager`, but `subscribe` fails);
  - the Chromecast button hidden in desktop;
  - a short "Desktop bridge & media backends" section in `AGENTS.md`.
- **PR-C: the media seam, with no behaviour change.**
  - `VoicePeer` / `VoiceMediaBackend` types.
  - The current AudioContext, gain and panner code moved verbatim into `lib/media/browserVoice.ts`.
  - `useWebRTCVoice` calls `backend.createPeer`, `acquireMic` and `attachSlot`.
  - `useSpeakingDetection` takes a level getter.
  - A one-line `DisplayCaptureBackend` hook in `useWebRTCScreen`.
  - Vitest coverage with a fake backend.
- **PR-D: fix voice settings.** Make `useVoiceSettings` a shared store. The output device, volumes and input gain are saved today but never applied.

### Phase 3: native voice (behind an "Experimental native voice" toggle and a localStorage kill switch)

- **Capture:** cpal capture → libwebrtc APM (AEC3/NS/AGC) → RNNoise (`nnnoiseless`) → `NativeAudioSource`.
- **Playout:** 12 slot sinks → mixer (gain, equal-power pan, distance; drift-corrected) → cpal. The mix is fed back as the echo canceller's reference.
- **Push-to-talk:** gated inside the engine.
  - Windows: `WH_KEYBOARD_LL`.
  - X11: XInput2.
  - Wayland: the GlobalShortcuts portal.
- **Also:** device hot-plug and a mic test.
- **IPC:**
  - The engine talks to the main process over a local socket.
  - The main process bridges that to the renderer through a `MessagePort`, checking origin and schema on every message.
  - The main-world adapter is injected by the preload, so it always ships with the matching engine.
- **Recovery:**
  - Engine crash: restart it and rejoin. After repeated crashes, fall back to browser voice for the session.
  - Page reload: the existing `sessionStorage.voiceSession` auto-rejoin.

### Phase 4: screen share with app audio

- **Windows:** WASAPI process loopback. Include the shared app's process tree, or exclude Chatter for full-screen shares.
- **Linux:** a PipeWire virtual sink, venmic-style, via `pipewire-rs`.
- **Routing:** the captured audio enters the renderer through MessagePort → AudioWorklet → `MediaStreamAudioDestinationNode`. Video stays on Chromium's hardware encoder.

### Phase 5: polish

Ducking ("attenuation"), game activity (`sysinfo` plus an allowlist, names only), auto-update, code signing, macOS.

## Risks

- **Echo for speaker users.** Once voice leaves Chromium, the engine's echo canceller only sees its own mix, not screen-share audio played by Chromium. Mitigation: an optional loopback reference; recommend headsets.
- **Wayland push-to-talk.** Portal shortcuts are keys only, depend on the desktop environment, and may be toggle-only.
- **Windows 10.** It has no process loopback before build 20348.
- **Antivirus heuristics.** Unsigned executables with keyboard hooks and audio capture look suspicious. Sign both executables.
- **Churn in LiveKit's bindings.** Mitigation: pin the version exactly and keep all binding code in `chatter-media`.
