//! Application audio capture for Chatter Desktop screen sharing.
//!
//! When a user shares a window or screen "with audio" (like Discord), the engine streams the
//! sound of either **one application** (the one being shared, [`Target::App`]) or **everything
//! except Chatter itself** ([`Target::AllExcept`], used for full-screen shares, so other
//! people's voices played by Chatter are not echoed back to them). This crate provides that
//! capture, normalised to what the engine's WebRTC audio source wants: 48 kHz stereo `f32`
//! in exact 10 ms chunks, delivered at a steady real-time pace (silence included).
//!
//! Backends:
//! - Windows 10 build 20348+ / Windows 11: WASAPI *process loopback*
//!   (`ActivateAudioInterfaceAsync` on `VAD\Process_Loopback`), which mixes a process tree's
//!   audio (or everything except a tree) inside the audio engine itself.
//! - Linux: PulseAudio (also served by `pipewire-pulse`). Each matching sink input is recorded
//!   through a per-stream monitor (`pa_stream_set_monitor_stream`) and the streams are mixed
//!   here; sink inputs are tracked live as apps open and close streams.
//! - Anything else: [`capabilities`] reports nothing, [`list_apps`] is empty and
//!   [`AppAudioCapture::start`] fails.
//!
//! The crate also provides [`Ducker`]: turning other applications down while people in the
//! call talk (Windows: per-session `ISimpleAudioVolume`; Linux: sink-input volumes). See the
//! `duck` module docs for the restore and user-override rules.

use std::panic::{catch_unwind, AssertUnwindSafe};

mod chunk;
mod duck;
#[cfg(any(windows, target_os = "linux", test))]
mod proc_tree;

#[cfg(target_os = "linux")]
mod pulse;
#[cfg(windows)]
mod wasapi;

pub use chunk::{CHANNELS, CHUNK_FRAMES, CHUNK_SAMPLES, SAMPLE_RATE};
pub use duck::{ducking_supported, Ducker};

/// An application that currently has (or recently had) an audio stream.
#[derive(Clone, Debug, serde::Serialize)]
pub struct AudioApp {
    /// Pid to pass to [`Target::App`]. This is the top-most process of the app's tree with the
    /// same executable (e.g. the main browser process, not its audio-service child); capture
    /// always includes the pid's whole process tree.
    pub pid: u32,
    /// Human-friendly name: the session's display name, the executable's version-resource
    /// description (Windows), the stream's `application.name` (Linux), or the executable's
    /// file stem.
    pub name: String,
}

/// What this machine can capture.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Capabilities {
    /// [`Target::App`] works.
    pub per_app: bool,
    /// [`Target::AllExcept`] works.
    pub all_except: bool,
}

/// Which audio to capture.
#[derive(Clone, Debug)]
pub enum Target {
    /// One application: `pid` and every process descended from it.
    App { pid: u32 },
    /// Everything except these processes and their descendants.
    ///
    /// On Windows the OS can exclude only *one* process tree, so the pid whose tree covers the
    /// most of the others is used (for Chatter, `[electron_main_pid, engine_pid]`: the engine
    /// is a child of Electron's main process, so excluding the main process's tree excludes
    /// both, plus the renderer and the Chromium audio service). An empty list excludes the
    /// current process's tree.
    AllExcept { pids: Vec<u32> },
}

/// Audio sink passed to backends: called with exactly [`CHUNK_SAMPLES`] samples.
pub(crate) type OnAudio = Box<dyn FnMut(&[f32]) + Send + 'static>;

/// A running backend capture. Dropped (after [`stop`](Self::stop)) by [`AppAudioCapture`].
pub(crate) trait Capture: Send {
    /// Stops and joins the capture thread. Called exactly once.
    fn stop(&mut self);
}

/// What the current OS supports.
pub fn capabilities() -> Capabilities {
    #[cfg(windows)]
    {
        wasapi::capabilities()
    }
    #[cfg(target_os = "linux")]
    {
        pulse::capabilities()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Capabilities {
            per_app: false,
            all_except: false,
        }
    }
}

/// Applications with an audio session/stream right now, excluding `exclude_pids` and their
/// process trees. Sorted by name (case-insensitive), deduplicated by pid. Empty on failure
/// (failures are logged): this feeds a picker, where "nothing found" is the right fallback.
pub fn list_apps(exclude_pids: &[u32]) -> Vec<AudioApp> {
    #[cfg(windows)]
    let apps = wasapi::list_apps(exclude_pids);
    #[cfg(target_os = "linux")]
    let apps = pulse::list_apps(exclude_pids);
    #[cfg(not(any(windows, target_os = "linux")))]
    let apps: Vec<AudioApp> = {
        let _ = exclude_pids;
        Vec::new()
    };
    sort_dedup(apps)
}

fn sort_dedup(mut apps: Vec<AudioApp>) -> Vec<AudioApp> {
    let mut seen = std::collections::HashSet::new();
    apps.retain(|a| seen.insert(a.pid));
    apps.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.pid.cmp(&b.pid))
    });
    apps
}

/// The process owning a window, from an Electron `desktopCapturer` source id
/// (`"window:<HWND>:0"` on Windows). `None` for screens, unknown windows and on other
/// platforms (X11 window ids would need `_NET_WM_PID`, which is unreliable and absent on
/// Wayland, where window ids are not exposed at all).
pub fn window_pid(source_id: &str) -> Option<u32> {
    #[cfg(windows)]
    {
        wasapi::window_pid(source_id)
    }
    #[cfg(not(windows))]
    {
        let _ = source_id;
        None
    }
}

/// A running capture. Dropping it stops capture and joins its thread.
pub struct AppAudioCapture {
    inner: Option<Box<dyn Capture>>,
}

impl AppAudioCapture {
    /// Starts capturing on its own thread.
    ///
    /// `on_audio` receives 48 kHz stereo interleaved `f32` (-1..1) in chunks of exactly
    /// [`CHUNK_FRAMES`] frames ([`CHUNK_SAMPLES`] samples, 10 ms). It keeps being called at
    /// real-time pace with zeros while the target is quiet (or has not started / has exited),
    /// so the consumer's clock keeps running. It runs on the capture thread and must not
    /// block for long; a panic in it stops the capture (the panic is caught and logged).
    ///
    /// Fails if the OS does not support the target (see [`capabilities`]) or the capture
    /// could not be set up; setup errors are reported here rather than later.
    pub fn start(
        target: Target,
        on_audio: impl FnMut(&[f32]) + Send + 'static,
    ) -> anyhow::Result<AppAudioCapture> {
        let mut on_audio = on_audio;
        let mut poisoned = false;
        // Contain panics so a buggy consumer cannot take the capture thread (and with it the
        // OS stream) down uncleanly; after one panic the callback is never called again.
        let guarded: OnAudio = Box::new(move |samples: &[f32]| {
            if poisoned {
                return;
            }
            if catch_unwind(AssertUnwindSafe(|| on_audio(samples))).is_err() {
                log::error!("appaudio: on_audio panicked; dropping further audio");
                poisoned = true;
            }
        });
        let inner = start_backend(target, guarded)?;
        Ok(AppAudioCapture { inner: Some(inner) })
    }
}

#[cfg(windows)]
fn start_backend(target: Target, on_audio: OnAudio) -> anyhow::Result<Box<dyn Capture>> {
    Ok(Box::new(wasapi::WasapiCapture::start(target, on_audio)?))
}

#[cfg(target_os = "linux")]
fn start_backend(target: Target, on_audio: OnAudio) -> anyhow::Result<Box<dyn Capture>> {
    Ok(Box::new(pulse::PulseCapture::start(target, on_audio)?))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn start_backend(_target: Target, _on_audio: OnAudio) -> anyhow::Result<Box<dyn Capture>> {
    anyhow::bail!("application audio capture is not supported on this platform")
}

impl Drop for AppAudioCapture {
    fn drop(&mut self) {
        if let Some(mut inner) = self.inner.take() {
            inner.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_and_dedup() {
        let a = |pid, name: &str| AudioApp {
            pid,
            name: name.into(),
        };
        let out = sort_dedup(vec![
            a(3, "zoom"),
            a(1, "Firefox"),
            a(3, "dup"),
            a(2, "discord"),
        ]);
        let got: Vec<(u32, &str)> = out.iter().map(|a| (a.pid, a.name.as_str())).collect();
        assert_eq!(got, vec![(2, "discord"), (1, "Firefox"), (3, "zoom")]);
    }

    #[test]
    fn capture_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<AppAudioCapture>();
    }
}
