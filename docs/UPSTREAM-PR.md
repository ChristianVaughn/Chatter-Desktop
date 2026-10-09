# Upstream PR: `desktop` → Sphyrna-029/Chatter `main`

The description for the one pull request from `ChristianVaughn/Chatter` branch `desktop`. Everything below the line is the PR body.

---

## Desktop app support, plus four bugs it turned up

This makes Chatter ready for [Chatter Desktop](https://github.com/ChristianVaughn/Chatter-Desktop), a Windows and Linux app. Like Discord's app, it opens your Chatter server's own UI in a window. It adds a native voice engine, system-wide push-to-talk, screen share with an app's audio, lowering other apps while people talk, and "Playing …" from the game you have open.

**In a browser nothing changes.** Every desktop path is switched on by `window.chatterDesktop`, which only the desktop app defines, and then only for each feature the app says it has. The client's protocol, signalling, retries and SDP handling stay where they are. The desktop app supplies media, the way a browser does.

Building it turned up four bugs that affect everyone today. They're fixed in their own commits, so they can be taken without the rest:

| Bug | Who it hits |
|---|---|
| WebRTC dies for good on a server running on Windows | anyone self-hosting on Windows |
| Closing a spare tab cuts your audio in a call you're in elsewhere | anyone with two tabs open |
| Calls are silent on Chrome 152 | current Chrome and Edge |
| Voice settings (output device, volumes, input gain) are saved but never applied | everyone |

### Commits

It's based on the current `main` (`b2a1a6a`). Every commit compiles (`cargo check --all-targets`), type-checks and passes the client tests on its own.

1. **`fix: keep WebRTC alive on a server running on Windows`** (server, 45 lines).
   - Windows reports an ICMP "port unreachable" as `WSAECONNRESET` on the UDP socket's next read.
   - webrtc-ice's mux treats that as fatal and stops reading, so every call on the host stays in "checking".
   - Fix: switch off `SIO_UDP_CONNRESET` on the mux socket. Windows only (`windows-sys`); Linux is untouched.
2. **`fix: closing a spare tab no longer cuts the audio of a call elsewhere`** (server).
   - `cleanup_disconnect` tore down the user's voice, screen and webcam media for *any* of their sockets closing, before the `holds_voice_session` check.
   - Fix: the media goes only with the connection holding the session, or with the user's last connection.
3. **`feat: let the desktop app identify itself, and push past an idle one`** (server).
   - The first WebSocket frame may carry `client: {kind: "desktop", version}`.
   - Push used to skip anyone with an open socket. A tray app is always connected, so phones would never get a push.
   - Now a desktop connection only stands in for a push while its user was active within the last 5 minutes.
4. **`feat: desktop bridge for the Chatter desktop app`** (client, `AGENTS.md`).
   - `lib/desktop/bridge.ts` is the typed contract.
   - In the app: the socket introduces itself, push reports `"desktop"`, and push-to-talk follows the app's system-wide key.
   - The in-window push-to-talk key now also releases on blur. Before, a keyup lost to a focus change left the mic open.
5. **`refactor: run voice on a media backend the desktop app can replace`** (client, no behaviour change in a browser).
   - `lib/media/` holds `VoicePeer` (the subset of `RTCPeerConnection` the hook uses) and `VoiceMediaBackend` (mic, playout graph, devices, mic test).
   - `browserVoice.ts` is the existing code, moved out of `useWebRTCVoice`.
   - The hook keeps signalling, retries, munging, moderation and slots.
   - Covered by `hooks/__tests__/voiceBackend.test.ts` with a fake backend.
6. **`fix: hear voice calls on current Chrome`** (client, 11 lines).
   - On Chromium 152, a remote track feeding both Web Audio and the muted `<audio>` element gives Web Audio silence.
   - Fix: Web Audio gets a clone of the track.
7. **`fix: apply voice settings to the call, and add desktop voice options`** (client).
   - `useVoiceSettings` was per hook instance, so the dialog's changes never reached the call. It is now one shared store, applied live.
   - A missing saved device falls back to the default.
   - A push-to-talk call starts muted.
   - In the app, the dialog also offers native voice, enhanced (RNNoise) noise suppression, and choosing the push-to-talk key.
8. **`feat: screen-share audio from the desktop app, and desktop game activity`** (client and server).
   - Screen share asks `lib/media` for its capture. The app adds the chosen app's audio as an ordinary track, so the share pipeline doesn't change.
   - New `game_activity` message, accepted from desktop connections only. It shows as the current game when Steam isn't reporting one, and respects "hide my game".
9. **`feat: desktop app option to lower other apps while people talk`** (client).
   - Voice & Audio → Output gains "Lower Other Apps" when the app offers it.
10. **`fix: review follow-ups for the desktop changes`**: fixes from a review pass over the above.
    - A reported game is cleared when the last desktop connection closes.
    - `game_activity` doesn't count as being active.
    - Two connections closing at once can't both skip teardown.
    - The browser mic's gain stage no longer sends silence when its audio context starts suspended.
    - Overlapping mic switches settle on the last one.

### How it was tested

- **Client:**
  - `npx vitest run`: 463 passed (39 files).
  - `tsc` is clean.
  - ESLint on the touched files: fewer errors than `main` (59 vs 71); the new files are clean.
- **Server:**
  - `cargo test --lib --bins`: 212 passed.
  - 2 fail, as they do on `main` on Windows: `media::tests::an_uploaded_mp4…` and `…mov…` compare a temp path with a bare file name.
  - `cargo test --test ws_contract` also fails on `main`: its helpers predate two-step TOTP registration.
- **End to end** (19 tests, Playwright driving the desktop app against a local server built from this branch):
  - browser voice audible both ways;
  - native voice both ways;
  - a call surviving the voice engine being killed;
  - screen share carrying a chosen app's tone;
  - system-wide push-to-talk;
  - deep links and the packaged installer.
- **Headless native clients:**
  - "spare tab" (commit 2): a third connection for a caller opens and closes mid-call, and the other side hears the caller throughout.
  - Game activity (commit 8) reached another user's presence.
- **Not yet run on Linux:** the desktop app's Linux build compiles and its tests pass in CI, but nobody has run it on a Linux desktop yet.

### Notes for agents working on Chatter

`AGENTS.md` has a new **Desktop App** section with the rules. In short:

- **Keep voice media behind `VoiceMediaBackend`.** Don't call `new RTCPeerConnection`, `getUserMedia` or Web Audio for voice from the hook. Add to `lib/media/types.ts` and implement in `browserVoice.ts`; the desktop app implements the same interface.
- **Treat `lib/desktop/bridge.ts` as a contract.** The desktop app vendors it. Add, don't change: new optional members, new feature strings, and versioned getters (`voiceBackend(1)`).
- **Check features before using them.** Gate on `hasDesktopFeature("…")`, never on `desktop` existing.

### Review hot spots

- `src/backend/ws/session.rs` `cleanup_disconnect`: who owns the media on close (commits 2 and 10).
- `src/backend/push.rs` `attended_users`: the idle rule for desktop connections.
- `client/src/hooks/useWebRTCVoice.ts`: the move onto the backend, and live settings.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
