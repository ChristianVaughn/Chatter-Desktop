//! End-to-end check of the Windows hook backend using synthesized input (SendInput).
//!
//! Ignored by default: it injects real, global key events (F13/F14, which nothing normally
//! uses) and needs an interactive desktop session. Run with:
//!     cargo test -p chatter-hotkeys --test sendinput -- --ignored
#![cfg(windows)]

use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use chatter_hotkeys::{Backend, Binding, HotkeyWatcher};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    VK_F13, VK_F14,
};

const WAIT: Duration = Duration::from_millis(500);
const QUIET: Duration = Duration::from_millis(150);

fn send_key(vk: u16, scan: u16, up: bool) {
    let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
    if vk == 0 {
        flags |= KEYEVENTF_SCANCODE;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: one valid INPUT with the right size.
    let sent = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
    assert_eq!(
        sent, 1,
        "SendInput was blocked (UIPI / no interactive desktop?)"
    );
}

fn expect(rx: &Receiver<bool>, want: bool) {
    match rx.recv_timeout(WAIT) {
        Ok(v) => assert_eq!(v, want),
        Err(_) => panic!("timed out waiting for {}", if want { "down" } else { "up" }),
    }
}

fn expect_nothing(rx: &Receiver<bool>) {
    if let Ok(v) = rx.recv_timeout(QUIET) {
        panic!("unexpected event {v}");
    }
}

/// The tests inject the same global keys, so they must not overlap when run in parallel.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn watcher() -> (HotkeyWatcher, Receiver<bool>) {
    let (tx, rx) = mpsc::channel();
    let w = HotkeyWatcher::start(move |v| {
        let _ = tx.send(v);
    })
    .expect("start");
    assert_eq!(w.backend(), Backend::WindowsHook);
    (w, rx)
}

#[test]
#[ignore = "injects global input; needs an interactive desktop"]
fn detects_press_release_and_dedupes_repeat() {
    let _serial = serial();
    let (w, rx) = watcher();
    w.set_binding(Some(Binding::new("F13")));

    // VK-only injection (scan code 0) -> VK fallback path.
    send_key(VK_F13, 0, false);
    expect(&rx, true);
    send_key(VK_F13, 0, false); // auto-repeat
    send_key(VK_F13, 0, false);
    send_key(VK_F13, 0, true);
    expect(&rx, false);
    expect_nothing(&rx);

    // Scan-code injection (how hardware reports it) -> scan code path.
    send_key(0, 0x64, false);
    expect(&rx, true);
    send_key(0, 0x64, true);
    expect(&rx, false);

    // Another key is never reported.
    send_key(VK_F14, 0, false);
    send_key(VK_F14, 0, true);
    expect_nothing(&rx);

    // Unbound: nothing.
    w.set_binding(None);
    send_key(VK_F13, 0, false);
    send_key(VK_F13, 0, true);
    expect_nothing(&rx);
    drop(w);
}

#[test]
#[ignore = "injects global input; needs an interactive desktop"]
fn rebinding_or_dropping_while_held_releases() {
    let _serial = serial();
    let (w, rx) = watcher();
    w.set_binding(Some(Binding::new("F13")));
    send_key(VK_F13, 0, false);
    expect(&rx, true);
    // Switch to F14 while F13 is held -> immediate release.
    w.set_binding(Some(Binding::new("F14")));
    expect(&rx, false);
    send_key(VK_F13, 0, true); // old key's release is ignored
    expect_nothing(&rx);

    send_key(VK_F14, 0, false);
    expect(&rx, true);
    // Clearing while held -> release.
    w.set_binding(None);
    expect(&rx, false);
    send_key(VK_F14, 0, true);
    expect_nothing(&rx);

    // Dropping while held -> final release delivered before drop returns.
    w.set_binding(Some(Binding::new("F14")));
    send_key(VK_F14, 0, false);
    expect(&rx, true);
    drop(w);
    send_key(VK_F14, 0, true);
    assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![false]);
}
