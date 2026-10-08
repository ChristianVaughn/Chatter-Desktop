# Native voice spike: results

_2026-10-08. Windows 11, local Chatter server (fork `fix/windows-webrtc-udp-connreset`, based on `6525666`)._

**Verdict: go.**
- Google libwebrtc, used through LiveKit's Rust bindings (`libwebrtc` 0.3.51) with no LiveKit server, speaks Chatter's voice protocol exactly as a browser does.
- It works in both directions, native ↔ native and native ↔ Chromium.
- Two bugs had to be fixed along the way, one in each stack (below).

## What was tested

`crates/chatter-testclient` is a headless client. It:
1. logs in, including the mandatory TOTP;
2. opens the chat WebSocket;
3. sends `voice_join`;
4. publishes an audio track from a WAV or a generated tone;
5. offers 12 receive-only slots;
6. munges the publish answer exactly as `client/src/lib/webrtc.ts` does (`maxaveragebitrate`, `usedtx`, `ptime 40`);
7. trickles ICE through the WebSocket;
8. records every slot to a WAV and reports.

The test accounts live in `.testclient/users.json`, which is gitignored.

```bash
cargo run -p chatter-testclient -- register alice
cargo run -p chatter-testclient -- setup-room alice bob
cargo run -p chatter-testclient -- voice alice --room '<room_id>' --tone-hz 440 --seconds 25
```

## Results

| Check | Result |
|---|---|
| Publish offer carries `ssrc-audio-level` (the server ranks speakers by it) | ✅ present; server reports the client in `voice_speaking` and assigns it a slot |
| Publish and subscribe connection time (local) | ✅ ~2.2 s each |
| 40 ms frames (`ptime 40` from the munged answer) | ✅ **25.0 packets/s** with a continuous tone |
| Channel bitrate cap (32 kbps default) | ✅ 12–26 kbps on a tone, with `maxaveragebitrate=32000` applied |
| DTX in silence | ✅ ~17 packets/s average for a tone that's on half the time |
| Native → native audio | ✅ A hears B's 660 Hz in slot 0, and B hears A's 440 Hz (both at about −17 dBFS) |
| Chromium → native audio | ✅ native client decodes the browser's 880 Hz in slot 0 at −15 dBFS, exactly the input level |
| Native → Chromium audio | ✅ Chromium receives the native client's Opus packets in slot 0 (packet counts; levels not measured because the test page doesn't play the audio) |
| Slot map and speaking indicators across clients | ✅ each side appears in the other's `voice_slot_map` and `voice_speaking` |
| Received stream continuity | ✅ 25 s session → 24.9 s slot WAV, with no gaps |
| Release binary size (test client, libwebrtc statically linked) | 26.5 MB |
| Platform audio device module (`AudioDeviceController`) reachable | ✅ through `PeerConnectionFactoryExt` (`playout_devices`, `set_recording_device`, `acquire_platform_adm`, …). Not used: Phase 3 plans its own capture/mix pipeline |

## Bugs found

### 1. Chatter server: all WebRTC dies on Windows hosts (fixed in the fork)

**Symptom.** Neither the native client nor Chromium could connect to a server running on Windows. ICE stayed in `checking`. The server's connectivity checks reached the client, but the client's checks were never answered.

**Cause.**
- Windows reports an ICMP "port unreachable" from an earlier UDP send as `WSAECONNRESET` on the socket's next receive.
- webrtc-ice 0.11's UDP mux (`udp_mux/mod.rs:206`) treats any receive error other than a timeout as fatal and stops reading.
- The first connectivity check the server sends to a candidate nobody listens on therefore permanently ends all WebRTC on that host.
- Linux doesn't report these errors on unconnected sockets, which is why Linux and Docker deployments are unaffected.

**Fix.** Disable `SIO_UDP_CONNRESET` on the mux socket before handing it over. This is Windows-only code in `src/backend/webrtc.rs`, plus `windows-sys` as a Windows-only dependency.
- Branch: `fix/windows-webrtc-udp-connreset`.
- It is a small, self-contained upstream PR that also helps anyone running Chatter on Windows with browsers only.

### 2. LiveKit bindings: `OfferOptions::default()` breaks receiving

**Symptom.** The server answered every voice slot with `a=inactive`, so no audio came back.

**Cause.**
- The bindings' default sets the legacy `offer_to_receive_audio` to an explicit `false` instead of leaving it unset.
- Unified-plan libwebrtc then strips the receive direction from every audio transceiver. The 12 slots go out `inactive`, and the publisher goes out `sendonly` where Chrome offers `sendrecv`.

**Fix.** Always pass `OfferOptions { offer_to_receive_audio: true, .. }`. This is now a rule for `chatter-media`.

## Notes for Phase 3

- **Build requirements.**
  - Windows needs VS 2022 Build Tools (C++20) and `+crt-static` (set in `.cargo/config.toml`).
  - Linux needs **clang 21 or newer**, because the prebuilt libwebrtc ships Chromium's own libc++. It also needs glib headers. CI installs clang from apt.llvm.org.
  - The prebuilt download is about 114 MB on Windows and about 156 MB on Linux, cached under `target/`.
- **Drive `NativeAudioSource` with `queue_size_ms = 0` from our own 10 ms clock.** The engine's mic callback will drive it the same way.
- **Buffer remote candidates until the answer is applied.** The server can send its first candidates just ahead of its answer.
- **The server drops our candidates if they arrive before the offer.** Send the offer first.
- **The server only accepts `voice_leave` and `voice_move` from the session-holding socket.** That is one reason the engine relays signalling through the renderer's WebSocket rather than opening its own.
- **Same-host testing on Windows also shows the server advertising mangled IPv6 host candidates** (byte-swapped `fe80::` and `fd7a:` addresses from webrtc-rs's interface enumeration). These are harmless because IPv4 pairs win, but they are worth reporting upstream to webrtc-rs.
