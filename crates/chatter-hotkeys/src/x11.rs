//! Linux/X11 backend: XInput2 raw key/button events selected on the root window.
//!
//! Raw events are delivered to every client that selects them, without grabbing anything, so
//! the bound key keeps working normally in the focused app. With XI >= 2.1 they are delivered
//! even while another client holds a grab. All other keys are dropped in [`Core::input`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{anyhow, Context};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, Window,
    WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::codes::{self, Resolved};
use crate::state::Core;
use crate::{Backend, Binding, Platform};

/// Target encoding: keys are X keycodes (evdev + 8, always < 256); buttons are `MOUSE_FLAG | n`.
const MOUSE_FLAG: u32 = 0x8000_0000;

pub(crate) struct X11Watcher {
    conn: Arc<RustConnection>,
    wake_window: Window,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl X11Watcher {
    pub(crate) fn start(core: Arc<Core>) -> anyhow::Result<Self> {
        let (conn, screen_num) = x11rb::connect(None).context("connecting to X server")?;
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("X screen {screen_num} not found"))?
            .root;

        if conn
            .extension_information(xinput::X11_EXTENSION_NAME)?
            .is_none()
        {
            return Err(anyhow!("XInputExtension not present"));
        }
        let version = conn.xinput_xi_query_version(2, 2)?.reply()?;
        if version.major_version < 2 {
            return Err(anyhow!(
                "XInput {}.{} is too old (need 2.x)",
                version.major_version,
                version.minor_version
            ));
        }

        let mask = xinput::XIEventMask::RAW_KEY_PRESS
            | xinput::XIEventMask::RAW_KEY_RELEASE
            | xinput::XIEventMask::RAW_BUTTON_PRESS
            | xinput::XIEventMask::RAW_BUTTON_RELEASE;
        conn.xinput_xi_select_events(
            root,
            &[xinput::EventMask {
                deviceid: u16::from(xinput::Device::ALL_MASTER),
                mask: vec![mask],
            }],
        )?
        .check()
        .context("selecting XI2 raw events")?;

        // Invisible InputOnly window that only exists so Drop can wake the blocking reader
        // with a ClientMessage.
        let wake_window = conn.generate_id()?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            wake_window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new(),
        )?
        .check()
        .context("creating wake window")?;
        conn.flush()?;

        let conn = Arc::new(conn);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let conn = conn.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("chatter-hotkeys-x11".into())
                .spawn(move || event_loop(&conn, &core, wake_window, &stop))
                .context("spawning X11 hotkey thread")?
        };
        Ok(X11Watcher {
            conn,
            wake_window,
            stop,
            thread: Some(thread),
        })
    }
}

fn event_loop(conn: &RustConnection, core: &Core, wake_window: Window, stop: &AtomicBool) {
    loop {
        let event = match conn.wait_for_event() {
            Ok(e) => e,
            Err(e) => {
                log::warn!("hotkeys: X11 connection error, stopping watcher: {e}");
                break;
            }
        };
        match event {
            Event::XinputRawKeyPress(e) => core.input(true, |t| t == e.detail),
            Event::XinputRawKeyRelease(e) => core.input(false, |t| t == e.detail),
            Event::XinputRawButtonPress(e) => core.input(true, |t| t == MOUSE_FLAG | e.detail),
            Event::XinputRawButtonRelease(e) => core.input(false, |t| t == MOUSE_FLAG | e.detail),
            Event::ClientMessage(e) if e.window == wake_window => {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
            }
            _ => {}
        }
    }
}

impl Platform for X11Watcher {
    fn backend(&self) -> Backend {
        Backend::X11
    }

    fn resolve(&self, binding: &Binding) -> Option<u32> {
        Some(match codes::resolve(&binding.code)? {
            Resolved::Key(k) => u32::from(k.evdev) + 8,
            Resolved::Mouse(m) => MOUSE_FLAG | m.x11_button(),
        })
    }

    fn binding_changed(&self, _binding: Option<&Binding>, _target: Option<u32>) {
        // Raw events for everything are already selected; filtering happens in Core.
    }

    fn stop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.stop.store(true, Ordering::SeqCst);
        let wake = ClientMessageEvent::new(32, self.wake_window, AtomEnum::NONE, [0u32; 5]);
        let sent = self
            .conn
            .send_event(false, self.wake_window, EventMask::NO_EVENT, wake)
            .map(|_| ())
            .and_then(|()| self.conn.flush());
        if let Err(e) = sent {
            // The reader already hit a connection error and exited (or is about to).
            log::debug!("hotkeys: X11 wake failed: {e}");
        }
        let _ = thread.join();
        let _ = self.conn.destroy_window(self.wake_window);
        let _ = self.conn.flush();
    }
}

impl Drop for X11Watcher {
    fn drop(&mut self) {
        self.stop();
    }
}
