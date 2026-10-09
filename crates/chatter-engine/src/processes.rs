//! Running programs, for game activity and the Settings window's "add a
//! game". Only programs a person would recognise: on Windows those with a
//! visible window; on Linux the user's own, minus desktop plumbing.

use serde::Serialize;
use std::collections::HashSet;
use std::sync::Mutex;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    /// The executable's file name, e.g. "Minecraft.exe" — what games match on.
    pub exe: String,
    /// The file name without its extension, for display.
    pub name: String,
}

static SYSTEM: Mutex<Option<System>> = Mutex::new(None);

/// The process that started us (the desktop app).
pub fn parent_pid() -> Option<u32> {
    let mut guard = SYSTEM.lock().unwrap_or_else(|e| e.into_inner());
    let system = guard.get_or_insert_with(System::new);
    let me = sysinfo::get_current_pid().ok()?;
    system.refresh_processes(ProcessesToUpdate::Some(&[me]), false);
    system.process(me)?.parent().map(|p| p.as_u32())
}

pub fn list() -> Vec<ProcessInfo> {
    let mut guard = SYSTEM.lock().unwrap_or_else(|e| e.into_inner());
    let system = guard.get_or_insert_with(System::new);
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_user(UpdateKind::OnlyIfNotSet),
    );
    let windowed = windowed_pids();
    let me = sysinfo::get_current_pid().ok();
    let my_user = me
        .and_then(|pid| system.process(pid))
        .and_then(|p| p.user_id().cloned());

    let mut seen = HashSet::new();
    let mut out: Vec<ProcessInfo> = system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            let exe_path = process.exe()?;
            let exe = exe_path.file_name()?.to_string_lossy().into_owned();
            let keep = match &windowed {
                Some(pids) => pids.contains(&pid.as_u32()),
                None => {
                    process.user_id() == my_user.as_ref()
                        && !is_plumbing(&exe, &exe_path.to_string_lossy())
                }
            };
            keep.then(|| ProcessInfo {
                pid: pid.as_u32(),
                name: exe_path
                    .file_stem()
                    .map_or_else(|| exe.clone(), |s| s.to_string_lossy().into_owned()),
                exe,
            })
        })
        .filter(|p| seen.insert(p.exe.to_ascii_lowercase()))
        .collect();
    out.sort_by_key(|p| p.name.to_ascii_lowercase());
    out
}

/// Processes owning a visible, titled top-level window.
#[cfg(windows)]
fn windowed_pids() -> Option<HashSet<u32>> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible,
    };

    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: lparam is the &mut HashSet passed to EnumWindows below, which
        // outlives the (synchronous) enumeration.
        let pids = unsafe { &mut *(lparam as *mut HashSet<u32>) };
        unsafe {
            if IsWindowVisible(hwnd) != 0 && GetWindowTextLengthW(hwnd) > 0 {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut pid);
                pids.insert(pid);
            }
        }
        1
    }

    let mut pids = HashSet::new();
    // SAFETY: the callback only touches `pids` for the duration of the call.
    unsafe { EnumWindows(Some(collect), &mut pids as *mut _ as LPARAM) };
    Some(pids)
}

#[cfg(not(windows))]
fn windowed_pids() -> Option<HashSet<u32>> {
    None
}

/// Desktop and session processes nobody would call a program they're using.
#[cfg_attr(windows, allow(dead_code))]
fn is_plumbing(exe: &str, path: &str) -> bool {
    const NAMES: &[&str] = &[
        "bash",
        "sh",
        "zsh",
        "fish",
        "dash",
        "sudo",
        "systemd",
        "dbus-daemon",
        "dbus-broker",
        "pipewire",
        "pipewire-pulse",
        "wireplumber",
        "pulseaudio",
        "Xwayland",
        "Xorg",
        "gnome-shell",
        "gnome-session-binary",
        "plasmashell",
        "kwin_wayland",
        "kwin_x11",
        "ksmserver",
        "kded6",
        "kded5",
        "xdg-desktop-portal",
        "xdg-document-portal",
        "xdg-permission-store",
        "at-spi-bus-launcher",
        "at-spi2-registryd",
        "gvfsd",
        "ssh-agent",
        "gpg-agent",
        "chatter-engine",
        "chatter-desktop",
    ];
    NAMES.contains(&exe)
        || path.starts_with("/usr/lib/")
        || path.starts_with("/usr/libexec/")
        || path.starts_with("/lib/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_something_including_no_duplicates() {
        let list = list();
        let mut exes: Vec<_> = list.iter().map(|p| p.exe.to_ascii_lowercase()).collect();
        let before = exes.len();
        exes.dedup();
        assert_eq!(before, exes.len());
    }

    #[test]
    fn plumbing_is_recognised() {
        assert!(is_plumbing("pipewire", "/usr/bin/pipewire"));
        assert!(is_plumbing("anything", "/usr/libexec/anything"));
        assert!(!is_plumbing("steam", "/usr/bin/steam"));
    }
}
