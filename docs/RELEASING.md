# Releasing

## Build

`npm run dist` (in `apps/desktop`) builds `chatter-engine` in release mode, then the app. The output lands in `apps/desktop/dist/`:

| Platform | Output | Updates itself |
|---|---|---|
| Windows | `Chatter-Setup-<version>.exe` (NSIS, per user, ~120 MB) | yes |
| Linux | `Chatter-<version>-x86_64.AppImage` | yes |
| Linux | `chatter-desktop_<version>_amd64.deb` | no; through the package manager |

The engine is copied to `resources/bin/` (`electron-builder.yml`, `extraResources`). `e2e/packaged.spec.ts` checks that a packaged build finds it and offers the native features.

Bump the version in `apps/desktop/package.json`. The Chatter UI has no version of its own here: it comes from the server.

## Publishing a release

1. Bump `version` in `apps/desktop/package.json`, commit, and push to `main`. CI builds and tests it.
2. Tag that commit with the same version and push the tag:

   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```

The tag starts `release.yml`. It checks that the tag matches the version in `package.json`, then finds CI's run for that commit on `main`, waiting for it if it's still running. It then publishes release `v0.2.0` from that run's installers, with notes generated from the commits since the last release. Nothing is rebuilt: what you release is exactly what CI tested. Installed apps pick it up on their next update check.

It stops with an error, and publishes nothing, if:
- **The tag doesn't match `package.json`:** tag the commit that bumps the version.
- **CI never built the commit** (it wasn't pushed to `main`): push it, then re-run the release workflow.
- **CI failed for it:** fix it, push, and tag the fixed commit instead. To move a tag: `git tag -d v0.2.0 && git push origin :v0.2.0`, then tag again.
- **CI's artifacts have expired** (after 14 days): re-run that CI run, then re-run the release workflow.

Edit the generated notes on GitHub afterwards to add the platform notes below.

## Updates

`electron-updater` reads the latest published release of `ChristianVaughn/Chatter-Desktop` (the `publish` block in `electron-builder.yml`); drafts are invisible to it. The installer's name has no spaces (`nsis.artifactName`), since GitHub turns them into dots on upload and `latest.yml` names it with dashes.

The app checks at launch and every 6 hours, downloads in the background, and installs on quit. It also offers to restart once an update is ready. People can turn this off in Settings. Development builds and `.deb` installs never update themselves.

## Signing

Unsigned builds work, but:
- **Windows:** SmartScreen warns on first run. Antivirus heuristics are also more suspicious of an unsigned app that installs keyboard hooks (push-to-talk) and captures process audio (screen share).
- **macOS:** not built yet. It would need signing and notarization anyway.

**Windows.** Sign both `Chatter.exe` and `chatter-engine.exe`. electron-builder signs everything it packages when given credentials, using either:
- a certificate: `CSC_LINK` (path or base64 `.pfx`) and `CSC_KEY_PASSWORD`; or
- Azure Trusted Signing: the cheapest option for individuals. Add `win.azureSignOptions` (endpoint, account, certificate profile) and provide `AZURE_TENANT_ID` / `AZURE_CLIENT_ID` / `AZURE_CLIENT_SECRET`.

The CI packaging step sets `CSC_IDENTITY_AUTO_DISCOVERY=false` so pull-request builds never try to sign.

## Platform notes for release notes

- **Windows 10:** no per-app screen-share audio; that needs Windows 11 or Server 2022 (build 20348). The picker simply offers no audio there. Everything else works.
- **Linux:**
  - **Shared libraries:** the engine needs `libpulse.so.0` (present on desktops with PulseAudio or PipeWire).
  - **Push-to-talk:**
    - X11: uses XInput2.
    - Wayland: uses the GlobalShortcuts portal (KDE, Hyprland, GNOME 48+). The desktop shows its own dialog to confirm the key.
  - **Not yet run on Linux:** the Linux builds are compiled, clippy-clean and unit-tested in CI. Push-to-talk, app audio and ducking haven't been run on a Linux machine yet; ask a Linux user to try them before a release.
- **Echo with speakers:** native voice cancels echo of the call itself. Screen-share audio played from speakers isn't in the echo canceller's reference, so recommend headphones for screen-share watching.
