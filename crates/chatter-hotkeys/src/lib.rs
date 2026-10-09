//! System-wide push-to-talk for Chatter Desktop.
//!
//! [`HotkeyWatcher`] watches exactly one configured key or mouse button and reports press /
//! release even while the app is unfocused. It only *observes* input (the key still reaches
//! whatever app has focus) and never reports anything about other keys: backends hand each
//! event to an internal filter that drops everything except the bound input.
//!
//! Backends:
//! - Windows: `WH_KEYBOARD_LL` / `WH_MOUSE_LL` hooks on a dedicated thread ([`Backend::WindowsHook`]).
//! - Linux/X11: XInput2 raw events on the root window ([`Backend::X11`]).
//! - Linux/Wayland: the XDG `GlobalShortcuts` portal ([`Backend::WaylandPortal`]).
//! - Anything else, or when nothing usable is found: [`Backend::Unsupported`] (a no-op watcher).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

mod codes;
mod state;

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
mod x11;

use state::Core;

/// A key as a DOM `KeyboardEvent.code` string (`"Backquote"`, `"KeyV"`, `"F13"`,
/// `"ControlLeft"`, `"Space"`, ...) or a mouse button `"Mouse3"` (middle), `"Mouse4"`, `"Mouse5"`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Binding {
    pub code: String,
}

impl Binding {
    pub fn new(code: impl Into<String>) -> Self {
        Binding { code: code.into() }
    }

    /// Human-readable label: "`", "V", "F13", "Left Ctrl", "Mouse 4", ...
    /// Unknown codes are returned verbatim.
    pub fn label(&self) -> String {
        codes::label(&self.code)
    }

    /// Whether the code is one this crate knows how to watch (on platforms that support it).
    pub fn is_known(&self) -> bool {
        codes::resolve(&self.code).is_some()
    }
}

/// Which mechanism the watcher ended up using.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    WindowsHook,
    X11,
    WaylandPortal,
    Unsupported,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::WindowsHook => "windows-hook",
            Backend::X11 => "x11",
            Backend::WaylandPortal => "wayland-portal",
            Backend::Unsupported => "unsupported",
        }
    }
}

/// Interface every platform backend implements.
pub(crate) trait Platform: Send + Sync {
    fn backend(&self) -> Backend;
    /// Encodes a binding as this backend's target id (see [`Core::input`]); `None` if this
    /// backend can't watch it.
    fn resolve(&self, binding: &Binding) -> Option<u32>;
    /// Called after the core's target changed (install/remove hooks, rebind portal shortcut...).
    fn binding_changed(&self, binding: Option<&Binding>, target: Option<u32>);
    /// Stops and joins the backend's threads. Called exactly once.
    fn stop(&mut self);
}

/// No-op backend (Windows always has its hook backend).
#[cfg(not(windows))]
pub(crate) struct Unsupported;

#[cfg(not(windows))]
impl Platform for Unsupported {
    fn backend(&self) -> Backend {
        Backend::Unsupported
    }
    fn resolve(&self, _: &Binding) -> Option<u32> {
        None
    }
    fn binding_changed(&self, _: Option<&Binding>, _: Option<u32>) {}
    fn stop(&mut self) {}
}

#[cfg(windows)]
fn start_platform(core: Arc<Core>) -> anyhow::Result<Box<dyn Platform>> {
    Ok(Box::new(windows::WindowsWatcher::start(core)?))
}

#[cfg(target_os = "linux")]
fn start_platform(core: Arc<Core>) -> anyhow::Result<Box<dyn Platform>> {
    Ok(linux_select(core))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn start_platform(_core: Arc<Core>) -> anyhow::Result<Box<dyn Platform>> {
    Ok(Box::new(Unsupported))
}

/// Linux backend selection.
///
/// `CHATTER_HOTKEYS_BACKEND=x11|portal|none` forces a choice. Otherwise, on a Wayland session
/// (`XDG_SESSION_TYPE=wayland`, or no session type but `WAYLAND_DISPLAY` set) only the portal
/// is used: XWayland raw
/// events only cover X clients, so an X11 watcher there would work only while an X window is
/// focused, which is worse than an honest "unsupported". On X11 sessions XInput2 is preferred,
/// with the portal as fallback (e.g. KDE on X11 also ships it).
#[cfg(target_os = "linux")]
fn linux_select(core: Arc<Core>) -> Box<dyn Platform> {
    use std::env;
    let non_empty = |k: &str| env::var_os(k).is_some_and(|v| !v.is_empty());

    let try_x11 = |core: &Arc<Core>| -> Option<Box<dyn Platform>> {
        if !non_empty("DISPLAY") {
            return None;
        }
        match x11::X11Watcher::start(core.clone()) {
            Ok(w) => Some(Box::new(w) as Box<dyn Platform>),
            Err(e) => {
                log::warn!("hotkeys: X11 backend unavailable: {e:#}");
                None
            }
        }
    };
    let try_portal = |core: &Arc<Core>| -> Option<Box<dyn Platform>> {
        match wayland::PortalWatcher::start(core.clone()) {
            Ok(w) => Some(Box::new(w) as Box<dyn Platform>),
            Err(e) => {
                log::warn!("hotkeys: GlobalShortcuts portal unavailable: {e:#}");
                None
            }
        }
    };

    let forced = env::var("CHATTER_HOTKEYS_BACKEND").unwrap_or_default();
    let chosen = match forced.to_ascii_lowercase().as_str() {
        "x11" => try_x11(&core),
        "portal" | "wayland" => try_portal(&core),
        "none" | "unsupported" => None,
        other => {
            if !other.is_empty() {
                log::warn!("hotkeys: ignoring unknown CHATTER_HOTKEYS_BACKEND={other:?}");
            }
            let session_type = env::var("XDG_SESSION_TYPE").unwrap_or_default();
            let wayland_session = session_type.eq_ignore_ascii_case("wayland")
                || (session_type.is_empty() && non_empty("WAYLAND_DISPLAY"));
            if wayland_session {
                try_portal(&core)
            } else {
                try_x11(&core).or_else(|| try_portal(&core))
            }
        }
    };
    chosen.unwrap_or_else(|| {
        log::warn!("hotkeys: no global hotkey backend available; push-to-talk works only in-app");
        Box::new(Unsupported) as Box<dyn Platform>
    })
}

/// Watches one configured key / mouse button system-wide. Dropping it unhooks and joins all
/// threads (emitting a final release if the key was held).
pub struct HotkeyWatcher {
    core: Arc<Core>,
    platform: Box<dyn Platform>,
    binding: Mutex<Option<Binding>>,
    dispatcher: Option<JoinHandle<()>>,
}

impl HotkeyWatcher {
    /// Starts the platform watcher on its own thread(s). `on_change(true)` on press, `(false)`
    /// on release; never called twice in a row with the same value (auto-repeat is deduped).
    ///
    /// The callback runs on a dedicated dispatcher thread (never inside an OS hook), so it may
    /// block briefly or call [`set_binding`](Self::set_binding). Nothing is watched until a
    /// binding is set.
    pub fn start(
        on_change: impl Fn(bool) + Send + Sync + 'static,
    ) -> anyhow::Result<HotkeyWatcher> {
        let (core, rx) = Core::new();
        let dispatcher = thread::Builder::new()
            .name("chatter-hotkeys-dispatch".into())
            .spawn(move || {
                for pressed in rx {
                    if catch_unwind(AssertUnwindSafe(|| on_change(pressed))).is_err() {
                        log::error!("hotkeys: on_change callback panicked");
                    }
                }
            })?;
        let platform = match start_platform(core.clone()) {
            Ok(p) => p,
            Err(e) => {
                core.shutdown();
                let _ = dispatcher.join();
                return Err(e);
            }
        };
        log::info!("hotkeys: using backend {}", platform.backend().as_str());
        Ok(HotkeyWatcher {
            core,
            platform,
            binding: Mutex::new(None),
            dispatcher: Some(dispatcher),
        })
    }

    /// Sets (or clears, with `None`) the watched input. If the previous binding was held, a
    /// release is emitted first so the mic can't stay open.
    pub fn set_binding(&self, binding: Option<Binding>) {
        let mut current = self.binding.lock().unwrap_or_else(|e| e.into_inner());
        if *current == binding {
            return;
        }
        let target = binding.as_ref().and_then(|b| {
            let t = self.platform.resolve(b);
            if t.is_none() && self.platform.backend() != Backend::Unsupported {
                log::warn!("hotkeys: {:?} can't be watched by this backend", b.code);
            }
            t
        });
        self.core.set_target(target);
        self.platform.binding_changed(binding.as_ref(), target);
        *current = binding;
    }

    pub fn backend(&self) -> Backend {
        self.platform.backend()
    }
}

impl Drop for HotkeyWatcher {
    fn drop(&mut self) {
        self.platform.stop();
        self.core.shutdown();
        if let Some(handle) = self.dispatcher.take() {
            // Dropping from inside the callback must not self-join.
            if handle.thread().id() != thread::current().id() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn binding_label_and_serde() {
        let b = Binding::new("ControlLeft");
        assert_eq!(b.label(), "Left Ctrl");
        assert!(b.is_known());
        assert!(!Binding::new("Mouse1").is_known());
        assert_eq!(Binding::new("Mouse4").label(), "Mouse 4");
        assert_eq!(Backend::WaylandPortal.as_str(), "wayland-portal");
    }

    #[test]
    fn watcher_starts_and_stops() {
        let (tx, rx) = mpsc::channel();
        let w = HotkeyWatcher::start(move |v| {
            let _ = tx.send(v);
        })
        .expect("start");
        w.set_binding(Some(Binding::new("F13")));
        w.set_binding(Some(Binding::new("Mouse4")));
        w.set_binding(Some(Binding::new("NotAKey")));
        w.set_binding(None);
        drop(w);
        // No input was produced, so nothing may have been reported.
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
    }
}
