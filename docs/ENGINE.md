# chatter-engine

The desktop app's native media process (`crates/chatter-engine`). The app starts it before the window opens, talks to it over stdin/stdout, and restarts it if it crashes. It logs to stderr; the app copies that to `<profile>/logs/engine.log`.

## What it does

| Area | Module | Notes |
|---|---|---|
| Peer connections | `peers.rs` | Google libwebrtc via LiveKit's bindings (`chatter-media`). Driven op by op by the Chatter client's voice hook through the page's `VoicePeer` stand-in. Chatter's protocol, retries and SDP munging stay in the client. |
| Microphone | `capture.rs` | cpal → mono → resample to 48 kHz → libwebrtc audio processing (echo cancellation, noise suppression, AGC) → RNNoise ("Enhanced") → input gain → speaking level → `NativeAudioSource`. One thread per mic, owning the device stream; device loss and device changes reopen it. |
| Speakers | `playout.rs` | One mixer thread owning the output device, paced by the device. Per-slot gain, plus the browser's equal-power / inverse-distance panning for spatial channels. The mix feeds echo cancellation as the far end, and is watched for speech (for ducking). |
| Devices | `devices.rs` | cpal `host:id` ids, `"default"` follows the system. Polled for changes every 2 s. |
| Push-to-talk | `chatter-hotkeys` | Watches one key or mouse button system-wide and reports press and release; never consumes input. Windows: low-level hooks, matched by scancode. X11: XInput2 raw events. Wayland: GlobalShortcuts portal. |
| App audio | `chatter-appaudio` | Per-app capture for screen sharing. Windows 11 / Server 2022+: process loopback (include the app's tree, or exclude Chatter's). Linux: PulseAudio/PipeWire per-sink-input monitors. |
| Ducking | `chatter-appaudio` | Turns other apps' session/stream volume down while the call is audible, and restores it afterwards. |
| Programs | `processes.rs` | Windowed programs (Windows) or the user's own (Linux), for game activity and "add a game". |
| Test devices | `fake.rs` | `CHATTER_ENGINE_FAKE_INPUT=tone:<hz>\|<wav>` and `CHATTER_ENGINE_FAKE_OUTPUT=<wav>` replace the mic and speakers in real time. |

## Protocol

Each frame is `u32 LE length | u8 kind | payload`. The kind is `J` (UTF-8 JSON) or `B` (binary: `u32 LE` stream id, then bytes). The TypeScript side is `apps/desktop/src/shared/engine.ts` and the Rust side is `protocol.rs`.

- **Requests** are `{ "id": n, "op": "…", ...args }`.
- **Responses** are `{ "re": n, "ok": true, "value": … }` or `{ "re": n, "ok": false, "error": "…" }`.
- **Order:** requests are handled one at a time, in order. The client depends on this: a peer must exist before its transceivers, and those before the offer.
- **Events** are `{ "ev": "…", ... }`.

### Operations

The **page** column marks what the Chatter page may call; the main process refuses everything else from it.

| Op | Args → value | Page |
|---|---|---|
| `devices.list` | → `{inputs, outputs}` of `{id, label}` | ✓ |
| `mic.open` | `micId, options` (MicOptions) | ✓ |
| `mic.update` | `micId, options` | ✓ |
| `mic.enable` | `micId, on` | ✓ |
| `mic.close` | `micId` | ✓ |
| `peer.create` | `peerId, iceServers[{urls[], username, credential}]` | ✓ |
| `peer.addRecvOnlyAudio` | `peerId` → transceiver index | ✓ |
| `peer.attachMic` | `peerId, micId` | ✓ |
| `peer.createOffer` | `peerId` → sdp | ✓ |
| `peer.setLocal` / `peer.setRemote` | `peerId, type, sdp` | ✓ |
| `peer.addIce` | `peerId, candidate, sdpMid, sdpMLineIndex` | ✓ |
| `peer.restartIce` | `peerId` | ✓ |
| `peer.setMaxBitrate` | `peerId, bps` | ✓ |
| `peer.getStats` | `peerId` → W3C-shaped stats dicts | ✓ |
| `peer.close` | `peerId` | ✓ |
| `playout.attach` | `slot, peerId, index` | ✓ |
| `playout.gain` | `slot, gain` | ✓ |
| `playout.position` | `slot, x, y, z` | ✓ |
| `playout.listener` | `x, y, z` | ✓ |
| `playout.output` | `deviceId, volume` | ✓ |
| `playout.close` | | ✓ |
| `test.start` | `options, outputDeviceId` | ✓ |
| `test.monitor` | `on` | ✓ |
| `test.gain` | `gain` | ✓ |
| `test.stop` | | ✓ |
| `appaudio.stop` | `stream` (only streams the page was given) | ✓ |
| `appaudio.caps` | → `{per_app, all_except}` | |
| `appaudio.list` | `excludePids` → `[{pid, name}]` | |
| `appaudio.windowPid` | `sourceId` → pid \| null | |
| `appaudio.start` | `target: {kind: "app", pid} \| {kind: "allExcept", pids}` → `{stream}` | |
| `ptt.set` | `binding: {code} \| null` → `{code, label} \| null` | |
| `ducking.set` | `amount` 0..1 | |
| `processes.list` | → `[{pid, exe, name}]` | |
| `session.reset` | Drops every peer, mic, test, capture and the playout | |

`MicOptions` is `{deviceId, echoCancellation, noiseSuppression: "none"|"browser"|"rnnoise", autoGainControl, gain}`.

### Events

| Event | Fields |
|---|---|
| `hello` | `version, features[] ("voice", "ptt", "app-audio", "ducking"), hotkeyBackend` |
| `peer.ice` | `peerId, candidate, sdpMid, sdpMLineIndex` |
| `peer.state` | `peerId, state` (W3C connection state) |
| `peer.track` | `peerId, index` |
| `mic.level` | `micId, level (RMS 0..1), speaking` (about 10 Hz; never speaking while muted) |
| `test.level` | `level` (0..100, the browser meter's scale) |
| `devices.changed` | |
| `ptt` | `down` |
| `engine.lost` | Sent by the app, not the engine, when the process died |

Binary stream frames carry app audio as 10 ms of 48 kHz interleaved stereo `f32 LE`.

## Security

The page belongs to whichever server the user picked, so it only gets the operations marked ✓. Everything that reveals or reaches beyond the call is kept in the main process: listing programs, starting captures, choosing the push-to-talk key, and ducking. Pickers and key capture are windows the app owns, so the page learns a choice only after the person makes it.

## Known engine behaviours

- **Offer options:** `OfferOptions::default()` in the bindings strips the receive direction from audio transceivers, so the engine always sets `offer_to_receive_audio` (see SPIKE-RESULTS.md).
- **Echo cancellation:** it sees the call's mix only. Screen-share audio played by Chromium isn't in its reference, so speaker users may hear that echoed. Headphones are unaffected.
- **Windows:** app audio needs Windows 11 or Server 2022 (build 20348). On Windows 10 the picker offers no audio.
- **Wayland:** push-to-talk uses the portal. The desktop decides the key and may ask the user to confirm it.
