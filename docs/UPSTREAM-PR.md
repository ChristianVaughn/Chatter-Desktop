# Upstream PR: `desktop` → Sphyrna-029/Chatter `main`

The description for the one pull request from `ChristianVaughn/Chatter` branch `desktop`. Everything below the line is the PR body.

---

## Desktop app support, plus four bugs it turned up

This makes Chatter ready for [Chatter Desktop](https://github.com/ChristianVaughn/Chatter-Desktop), a desktop app for Windows and Linux. Like Discord's app, it opens your Chatter server's own UI in a window, so the interface is always whatever the server serves. On top of that it adds what a browser tab can't do:

- **A native voice engine:** Google's libwebrtc in a separate Rust process. It brings stronger noise suppression (RNNoise), echo cancellation and a choice of output device.
- **System-wide push-to-talk:** works while a game has focus.
- **Screen share with one app's audio:** for example a game's sound, without the call being sent back into itself.
- **"Lower Other Apps":** turns games and music down while people talk.
- **"Playing …" status:** from the game you have open.
- **Desktop basics:** tray, native notifications, unread badge, `chatter://` links, automatic updates.

**In a browser nothing changes.** Every desktop path is switched on by `window.chatterDesktop`, which only the desktop app defines, and then only for each feature the app says it has. Chatter's protocol, signalling, retries and SDP handling all stay in the client. The desktop app only supplies media, the way a browser does.

Building it turned up four bugs that affect everyone today. Each is fixed in its own commit, so you can take them without the rest:

| Bug | Who it hits |
|---|---|
| WebRTC dies for good on a server running on Windows | anyone self-hosting on Windows |
| Closing a spare tab cuts your audio in a call you're in elsewhere | anyone with two tabs open |
| Calls are silent on Chrome 152 | current Chrome and Edge |
| Voice settings (output device, volumes, input gain) are saved but never applied | everyone |

### Could you test it on Linux?

I don't have a Linux machine, so everything below was tested on Windows 11. The Linux build compiles and has unit tests, and the Chatter-Desktop repo's GitHub Actions builds and tests it on Ubuntu 22.04. Nobody has used it on a real Linux desktop yet, and you're the best person to do that.

**1. Get the app.** Open the latest [Actions run](https://github.com/ChristianVaughn/Chatter-Desktop/actions) and download the `chatter-desktop-linux` artifact. Then either:

```bash
sudo apt install ./chatter-desktop_*_amd64.deb
```

or, on any distro:

```bash
chmod +x Chatter-*.AppImage && ./Chatter-*.AppImage
```

The AppImage needs `libfuse2` on some distros. To build it yourself instead, see the Chatter-Desktop README.

**2. Run a server from this branch.** The desktop features need this PR's client. Run it locally (`cargo run` after `cd client && npm run build`) or put it on a test instance, then point the app at it on first launch.

**3. Try these, and tell me what breaks:**

- [ ] **Sign in:** it stays signed in after quitting and reopening.
- [ ] **Native voice:** in Voice & Audio, check "Native Voice Engine" is on (it applies from your next call). Then, in a call with someone in a browser, both of you should hear each other. Noise suppression should also offer "Enhanced".
- [ ] **Push-to-talk outside the window:** turn on Push to Talk in Voice & Audio and choose a Push to Talk Key. Then switch to another app and hold the key.
  - On X11 it should just work.
  - On Wayland your desktop should show its own dialog to confirm the shortcut. This needs the GlobalShortcuts portal (KDE, Hyprland, GNOME 48+).
- [ ] **Screen share with app audio:** share a window while something plays sound. In the picker, choose that app under Audio. The viewer should hear it, but not the call.
- [ ] **"Lower Other Apps":** under Voice & Audio → Output, set it to about 80% and play music. The music should drop while someone talks and come back after.
- [ ] **"Playing …":** open a game and your status should show it. For a game it doesn't recognise, open the app's own Settings (tray menu) and use "Add a running program…".
- [ ] **Notifications:** close the window to the tray, have someone message you, and a desktop notification appears. Clicking it brings the window back.

If something fails, the logs folder has `engine.log`, which is the most useful thing to send. To open it, press Alt to show the menu bar, then choose Help → "Open logs folder".

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

### How it was tested (Windows 11)

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
  - native voice both ways, including "Lower Other Apps";
  - a call surviving the voice engine being killed;
  - screen share carrying a chosen app's audio;
  - system-wide push-to-talk;
  - deep links and the packaged installer.
- **Headless native clients:**
  - "Spare tab" (commit 2): a third connection for a caller opens and closes mid-call, and the other side hears the caller throughout.
  - Game activity (commit 8) reached another user's presence.

### Notes for agents working on Chatter

`AGENTS.md` has a new **Desktop App** section with the rules. In short:

- **Keep voice media behind `VoiceMediaBackend`.** Don't call `new RTCPeerConnection`, `getUserMedia` or Web Audio for voice from the hook. Add to `lib/media/types.ts` and implement in `browserVoice.ts`; the desktop app implements the same interface.
- **Treat `lib/desktop/bridge.ts` as a contract.** The desktop app vendors it. Change it additively only: new optional members, new feature strings, versioned getters (`voiceBackend(1)`).
- **Check features before using them.** Gate on `hasDesktopFeature("…")`, never on `desktop` existing.

### Review hot spots

- `src/backend/ws/session.rs` `cleanup_disconnect`: who owns the media on close (commits 2 and 10).
- `src/backend/push.rs` `attended_users`: the idle rule for desktop connections.
- `client/src/hooks/useWebRTCVoice.ts`: the move onto the backend, and live settings.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
