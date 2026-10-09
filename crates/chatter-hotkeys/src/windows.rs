//! Windows backend: low-level keyboard/mouse hooks on a dedicated message-loop thread.
//!
//! Only the hook needed for the current binding is installed (keyboard *or* mouse, none when
//! unbound). The hook procs never swallow input: they always return `CallNextHookEx`.
//!
//! Key matching: DOM `code` is positional, defined by the scan code, so physical key events
//! are matched on (scan code, extended flag), which is keyboard-layout independent and tells
//! left/right modifiers apart (`0x1D` vs `0xE01D`). Injected events (`SendInput`, remappers)
//! often carry no or a sloppy scan code, so for those the virtual-key code also matches (the
//! LL hook reports left/right-specific VKs such as `VK_LSHIFT`).

use std::cell::RefCell;
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context};
use windows_sys::Win32::Foundation::{GetLastError, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED,
    LLKHF_UP, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_QUIT, WM_USER, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1,
    XBUTTON2,
};

use crate::codes::{self, MouseButton, Resolved};
use crate::state::Core;
use crate::{Backend, Binding, Platform};

/// Target encoding: keys are `vk << 16 | scan` (vk < 0x100, so bit 31 stays clear); mouse
/// buttons are `MOUSE_FLAG | n` (n = 3/4/5).
const MOUSE_FLAG: u32 = 0x8000_0000;
const WM_RECONFIGURE: u32 = WM_APP + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HookKind {
    None,
    Keyboard,
    Mouse,
}

type Cmd = (HookKind, Sender<()>);

pub(crate) struct WindowsWatcher {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    cmd_tx: Sender<Cmd>,
}

thread_local! {
    /// Set on the hook thread only; the LL hook procs run on the thread that installed them.
    static CORE: RefCell<Option<Arc<Core>>> = const { RefCell::new(None) };
}

impl WindowsWatcher {
    pub(crate) fn start(core: Arc<Core>) -> anyhow::Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let (ready_tx, ready_rx) = mpsc::channel::<u32>();
        let thread = std::thread::Builder::new()
            .name("chatter-hotkeys-hook".into())
            .spawn(move || hook_thread(core, cmd_rx, ready_tx))
            .context("spawning hotkey hook thread")?;
        let thread_id = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow!("hotkey hook thread failed to start"))?;
        Ok(WindowsWatcher {
            thread_id,
            thread: Some(thread),
            cmd_tx,
        })
    }
}

impl Platform for WindowsWatcher {
    fn backend(&self) -> Backend {
        Backend::WindowsHook
    }

    fn resolve(&self, binding: &Binding) -> Option<u32> {
        Some(match codes::resolve(&binding.code)? {
            Resolved::Key(k) => encode_key(k),
            Resolved::Mouse(MouseButton::Middle) => MOUSE_FLAG | 3,
            Resolved::Mouse(MouseButton::Back) => MOUSE_FLAG | 4,
            Resolved::Mouse(MouseButton::Forward) => MOUSE_FLAG | 5,
        })
    }

    fn binding_changed(&self, _binding: Option<&Binding>, target: Option<u32>) {
        let kind = match target {
            None => HookKind::None,
            Some(t) if t & MOUSE_FLAG != 0 => HookKind::Mouse,
            Some(_) => HookKind::Keyboard,
        };
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.cmd_tx.send((kind, ack_tx)).is_err() {
            log::error!("hotkeys: hook thread is gone");
            return;
        }
        // SAFETY: plain Win32 call; the thread id belongs to our hook thread.
        if unsafe { PostThreadMessageW(self.thread_id, WM_RECONFIGURE, 0, 0) } == 0 {
            log::error!("hotkeys: PostThreadMessageW failed: {}", unsafe {
                GetLastError()
            });
            return;
        }
        // Wait so the hook is live when set_binding returns (keeps behavior deterministic).
        if ack_rx.recv_timeout(Duration::from_secs(2)).is_err() {
            log::warn!("hotkeys: hook thread did not acknowledge reconfigure");
        }
    }

    fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            // SAFETY: plain Win32 call. If the thread already exited this fails harmlessly.
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
            let _ = thread.join();
        }
    }
}

impl Drop for WindowsWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Hooks {
    keyboard: HHOOK,
    mouse: HHOOK,
}

impl Hooks {
    fn apply(&mut self, kind: HookKind) {
        set_hook(
            &mut self.keyboard,
            kind == HookKind::Keyboard,
            WH_KEYBOARD_LL,
            keyboard_proc,
        );
        set_hook(
            &mut self.mouse,
            kind == HookKind::Mouse,
            WH_MOUSE_LL,
            mouse_proc,
        );
    }
}

type HookFn = unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT;

fn set_hook(slot: &mut HHOOK, want: bool, id: i32, proc_: HookFn) {
    if want && slot.is_null() {
        // SAFETY: installs a global LL hook whose proc is a plain fn living for the program.
        let hook = unsafe { SetWindowsHookExW(id, Some(proc_), GetModuleHandleW(null()), 0) };
        if hook.is_null() {
            log::error!("hotkeys: SetWindowsHookExW({id}) failed: {}", unsafe {
                GetLastError()
            });
        }
        *slot = hook;
    } else if !want && !slot.is_null() {
        // SAFETY: the handle came from SetWindowsHookExW on this thread.
        unsafe { UnhookWindowsHookEx(*slot) };
        *slot = null_mut();
    }
}

fn hook_thread(core: Arc<Core>, cmd_rx: Receiver<Cmd>, ready_tx: Sender<u32>) {
    CORE.with(|c| *c.borrow_mut() = Some(core));
    let mut msg = MSG::default();
    // SAFETY: plain Win32 calls with valid out-pointers.
    unsafe {
        // Force creation of this thread's message queue so PostThreadMessageW works.
        PeekMessageW(&mut msg, null_mut(), WM_USER, WM_USER, PM_NOREMOVE);
        let _ = ready_tx.send(GetCurrentThreadId());
    }
    drop(ready_tx);

    let mut hooks = Hooks {
        keyboard: null_mut(),
        mouse: null_mut(),
    };
    loop {
        // SAFETY: valid out-pointer; null hwnd = all messages for this thread. Hook procs run
        // from inside this call.
        let r = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
        if r == 0 || r == -1 {
            break; // WM_QUIT or error
        }
        if msg.hwnd.is_null() && msg.message == WM_RECONFIGURE {
            while let Ok((kind, ack)) = cmd_rx.try_recv() {
                hooks.apply(kind);
                let _ = ack.send(());
            }
        }
    }
    hooks.apply(HookKind::None);
    CORE.with(|c| c.borrow_mut().take());
}

fn with_core(f: impl FnOnce(&Core)) {
    CORE.with(|c| {
        if let Ok(guard) = c.try_borrow() {
            if let Some(core) = guard.as_ref() {
                f(core);
            }
        }
    });
}

fn encode_key(k: &codes::KeyDef) -> u32 {
    (u32::from(k.win_vk) << 16) | u32::from(k.win_scan)
}

/// Does a keyboard LL event match the encoded target? See the module docs for the rules.
fn key_matches(target: u32, vk: u32, scan: u32, flags: u32) -> bool {
    if target & MOUSE_FLAG != 0 {
        return false;
    }
    let (t_vk, t_scan) = (target >> 16, target & 0xFFFF);
    let injected = flags & LLKHF_INJECTED != 0;
    if scan != 0 {
        let ext = if flags & LLKHF_EXTENDED != 0 {
            0xE000
        } else {
            0
        };
        if (scan | ext) == t_scan {
            return true;
        }
        if !injected {
            return false;
        }
    }
    vk == t_vk
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam != 0 {
        // SAFETY: for HC_ACTION, lparam points to a KBDLLHOOKSTRUCT valid for this call.
        let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let down = kb.flags & LLKHF_UP == 0;
        let (vk, scan, flags) = (kb.vkCode, kb.scanCode, kb.flags);
        with_core(|core| core.input(down, |t| key_matches(t, vk, scan, flags)));
    }
    // Observe only: always pass the event on.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam != 0 {
        let event = match wparam as u32 {
            WM_MBUTTONDOWN => Some((3, true)),
            WM_MBUTTONUP => Some((3, false)),
            msg @ (WM_XBUTTONDOWN | WM_XBUTTONUP) => {
                // SAFETY: for HC_ACTION, lparam points to an MSLLHOOKSTRUCT valid for this call.
                let ms = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
                match (ms.mouseData >> 16) as u16 {
                    XBUTTON1 => Some((4, msg == WM_XBUTTONDOWN)),
                    XBUTTON2 => Some((5, msg == WM_XBUTTONDOWN)),
                    _ => None,
                }
            }
            _ => None, // moves, wheel, left/right clicks: ignored without further work
        };
        if let Some((button, down)) = event {
            with_core(|core| core.input(down, |t| t == MOUSE_FLAG | button));
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(code: &str) -> u32 {
        let k = codes::lookup_key(code).unwrap();
        encode_key(k)
    }

    #[test]
    fn physical_keys_match_by_scan_code() {
        let t = target("ControlLeft");
        assert!(key_matches(t, 0xA2, 0x1D, 0));
        // Right Ctrl: same scan byte but extended.
        assert!(!key_matches(t, 0xA3, 0x1D, LLKHF_EXTENDED));
        // AltGr's fake LCtrl carries scan 0x21D: must not trigger Left Ctrl.
        assert!(!key_matches(t, 0xA2, 0x21D, 0));
        assert!(key_matches(
            target("ControlRight"),
            0xA3,
            0x1D,
            LLKHF_EXTENDED
        ));

        // Layout independence: physical Q position reports VK_A on AZERTY; still KeyQ.
        assert!(key_matches(target("KeyQ"), 0x41, 0x10, 0));
        assert!(!key_matches(target("KeyA"), 0x41, 0x10, 0));

        // NumLock off: Numpad1 reports VK_END but the non-extended scan code 0x4F.
        assert!(key_matches(target("Numpad1"), 0x23, 0x4F, 0));
        assert!(!key_matches(target("End"), 0x23, 0x4F, 0));
        assert!(key_matches(target("End"), 0x23, 0x4F, LLKHF_EXTENDED));

        // Enter vs NumpadEnter.
        assert!(key_matches(
            target("NumpadEnter"),
            0x0D,
            0x1C,
            LLKHF_EXTENDED
        ));
        assert!(!key_matches(target("Enter"), 0x0D, 0x1C, LLKHF_EXTENDED));

        // Pause vs NumLock share scan byte 0x45.
        assert!(key_matches(target("Pause"), 0x13, 0x45, 0));
        assert!(key_matches(target("NumLock"), 0x90, 0x45, LLKHF_EXTENDED));
        assert!(!key_matches(target("Pause"), 0x90, 0x45, LLKHF_EXTENDED));
    }

    #[test]
    fn injected_keys_fall_back_to_vk() {
        let t = target("F13");
        assert!(key_matches(t, 0x7C, 0, LLKHF_INJECTED));
        assert!(key_matches(t, 0x7C, 0, 0)); // no scan code at all
        assert!(key_matches(t, 0x7C, 0x64, LLKHF_INJECTED));
        assert!(!key_matches(t, 0x7D, 0, LLKHF_INJECTED));
        // Injected arrow without the extended flag (common remapper bug) still matches by VK.
        assert!(key_matches(
            target("ArrowRight"),
            0x27,
            0x4D,
            LLKHF_INJECTED
        ));
        assert!(key_matches(target("ShiftRight"), 0xA1, 0, LLKHF_INJECTED));
        assert!(!key_matches(target("ShiftLeft"), 0xA1, 0, LLKHF_INJECTED));
    }

    #[test]
    fn mouse_targets_never_match_keys() {
        assert!(!key_matches(MOUSE_FLAG | 4, 0x05, 0, LLKHF_INJECTED));
        for k in codes::KEYS {
            assert_eq!(
                encode_key(k) & MOUSE_FLAG,
                0,
                "{} collides with MOUSE_FLAG",
                k.code
            );
        }
    }
}
