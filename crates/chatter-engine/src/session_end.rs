//! Putting things back when the OS ends the session (shutdown, restart, sign-out).
//!
//! The app's own quit closes our stdin and waits for us, but a shutdown doesn't go through it:
//! Electron skips `before-quit` then, and the engine is simply ended, leaving other apps
//! ducked. Windows: a hidden top-level window runs the hook on `WM_ENDSESSION`, before the
//! process is ended. Unix: the session ends with SIGTERM (SIGHUP when its terminal goes, SIGINT
//! for Ctrl+C in a dev shell); the hook runs, then the engine exits.

/// Runs `on_end` once when the OS ends the session, before the process is ended.
#[cfg(windows)]
pub fn watch(on_end: impl Fn() + Send + Sync + 'static) {
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW, MSG,
        WM_ENDSESSION, WNDCLASSW,
    };

    static HOOK: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    if HOOK.set(Box::new(on_end)).is_err() {
        return;
    }

    unsafe extern "system" fn proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // wParam is false when another app called the shutdown off.
        if msg == WM_ENDSESSION && wparam != 0 {
            log::info!("the system is ending the session");
            if let Some(hook) = HOOK.get() {
                hook();
            }
            return 0;
        }
        // SAFETY: the arguments are the ones this window was sent.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    let spawned = std::thread::Builder::new()
        .name("session-end".into())
        .spawn(|| {
            let class: Vec<u16> = "ChatterEngineSessionEnd\0".encode_utf16().collect();
            // SAFETY: plain Win32 calls; `class` outlives the window, which lives as long as
            // this thread, and the message loop runs on the thread that created the window.
            unsafe {
                let instance = GetModuleHandleW(std::ptr::null());
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(proc),
                    hInstance: instance,
                    lpszClassName: class.as_ptr(),
                    ..std::mem::zeroed()
                };
                if RegisterClassW(&wc) == 0 {
                    log::warn!("can't watch for the session ending: RegisterClassW failed");
                    return;
                }
                // Top-level rather than message-only: only those are told the session is
                // ending. Never shown.
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                if hwnd.is_null() {
                    log::warn!("can't watch for the session ending: CreateWindowExW failed");
                    return;
                }
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    DispatchMessageW(&msg);
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("can't watch for the session ending: {e}");
    }
}

/// Runs `on_end` once when the OS ends the session, then exits. Must be called from within
/// the tokio runtime.
#[cfg(unix)]
pub fn watch(on_end: impl Fn() + Send + Sync + 'static) {
    use tokio::signal::unix::{signal, SignalKind};
    let signals = (
        signal(SignalKind::terminate()),
        signal(SignalKind::hangup()),
        signal(SignalKind::interrupt()),
    );
    let (Ok(mut term), Ok(mut hup), Ok(mut int)) = signals else {
        log::warn!("can't watch for the session ending: no signal handlers");
        return;
    };
    tokio::spawn(async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = hup.recv() => {}
            _ = int.recv() => {}
        }
        log::info!("asked to stop by the system");
        on_end();
        std::process::exit(0);
    });
}

#[cfg(not(any(windows, unix)))]
pub fn watch(_on_end: impl Fn() + Send + Sync + 'static) {}
