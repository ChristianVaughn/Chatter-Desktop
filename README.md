# Chatter Desktop

Desktop client for [Chatter](https://github.com/Sphyrna-029/Chatter), the self-hosted chat app, for Windows and Linux.

The app is an Electron shell that loads your Chatter server and adds desktop features:
- tray, with close-to-tray;
- native notifications;
- unread badge;
- screen-share picker.

A native Rust voice engine built on Google libwebrtc is on the way. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design and roadmap, and [docs/SPIKE-RESULTS.md](docs/SPIKE-RESULTS.md) for the native voice proof of concept.

## Layout

| Path | What |
|---|---|
| `apps/desktop` | Electron shell (TypeScript, electron-vite, electron-builder) |
| `crates/chatter-media` | Native media layer: facade over libwebrtc |
| `crates/chatter-testclient` | Headless Chatter voice client used to test the native stack |
| `scripts/run-chatter-server.mjs` | Runs a local Chatter server from `../Chatter` (or `$CHATTER_DIR`) |

## Develop

**Requirements**
- Node 22.
- Rust (version pinned in `rust-toolchain.toml`).
- On Windows: VS 2022 Build Tools with the C++ workload.
- On Linux: clang 21 or newer and glib headers.

**Local server**

You need a local Chatter server (MongoDB plus a checkout of the Chatter repo next to this one):

```bash
node scripts/run-chatter-server.mjs
```

**Desktop app** (hot reload):

```bash
npm install
npm run dev
```

Point it at `localhost:8000` on first launch.

**Tests**

```bash
npm run build && CHATTER_E2E_SERVER=http://localhost:8000 npm run test:e2e -w apps/desktop
```

```bash
cargo test --workspace
```

**Package**

```bash
npm run dist
```

Produces an NSIS installer on Windows, and AppImage + deb on Linux.

**Native voice client**

```bash
cargo run -p chatter-testclient -- --help
```

## License

GPL-3.0-or-later, same as Chatter.
