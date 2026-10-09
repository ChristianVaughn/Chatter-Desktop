//! Process-tree helpers shared by the backends (pure functions over a process snapshot).
//!
//! Both OS capture mechanisms think in process *trees*: Windows process loopback includes or
//! excludes a whole tree, and on Linux we match PulseAudio streams against a tree ourselves.
//! Trees matter because audio is rarely played by the process the user thinks of as "the
//! app": Chromium/Electron (Chrome, Discord, Chatter itself) play from an audio-service child
//! process, and games often play from a helper.

use std::collections::{HashMap, HashSet};

/// One process in a snapshot.
#[derive(Clone, Debug)]
pub struct ProcEntry {
    pub pid: u32,
    /// Parent pid; 0 when unknown / none.
    pub ppid: u32,
    /// Executable name as the OS reports it (`"chrome.exe"`, `"firefox"`).
    pub exe: String,
}

/// Snapshot indexed by pid.
pub struct ProcTable {
    by_pid: HashMap<u32, ProcEntry>,
}

impl ProcTable {
    pub fn new(entries: impl IntoIterator<Item = ProcEntry>) -> Self {
        ProcTable {
            by_pid: entries.into_iter().map(|e| (e.pid, e)).collect(),
        }
    }

    pub fn get(&self, pid: u32) -> Option<&ProcEntry> {
        self.by_pid.get(&pid)
    }

    /// Parent of `pid`, if it is in the snapshot (and is not `pid` itself: the idle/system
    /// processes on Windows are their own parent).
    fn parent(&self, pid: u32) -> Option<&ProcEntry> {
        let e = self.by_pid.get(&pid)?;
        if e.ppid == 0 || e.ppid == pid {
            return None;
        }
        self.by_pid.get(&e.ppid)
    }

    /// Whether `pid` is `root` or (transitively) a child of it. Cycle-safe: Windows parent
    /// pids are not invalidated when the parent exits, so a reused pid can form a loop.
    pub fn in_tree(&self, pid: u32, root: u32) -> bool {
        let mut cur = pid;
        for _ in 0..self.by_pid.len() + 1 {
            if cur == root {
                return true;
            }
            match self.parent(cur) {
                Some(p) => cur = p.pid,
                None => return false,
            }
        }
        false
    }

    /// `roots` plus all their descendants (roots are included even if not in the snapshot).
    pub fn trees(&self, roots: &[u32]) -> HashSet<u32> {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for e in self.by_pid.values() {
            if e.ppid != e.pid {
                children.entry(e.ppid).or_default().push(e.pid);
            }
        }
        let mut out: HashSet<u32> = HashSet::new();
        let mut stack: Vec<u32> = roots.to_vec();
        while let Some(pid) = stack.pop() {
            if out.insert(pid) {
                if let Some(kids) = children.get(&pid) {
                    stack.extend(kids.iter().copied());
                }
            }
        }
        out
    }

    /// The top-most ancestor of `pid` reachable through parents with the *same executable
    /// name*, e.g. a Chrome audio-service `chrome.exe` maps to the browser's main `chrome.exe`.
    /// Listing that pid lets one entry stand for the whole app, and a tree-inclusive capture of
    /// it still covers the process that actually plays.
    pub fn app_root(&self, pid: u32) -> u32 {
        let Some(mut cur) = self.by_pid.get(&pid) else {
            return pid;
        };
        for _ in 0..self.by_pid.len() {
            match self.parent(cur.pid) {
                Some(p) if !p.exe.is_empty() && p.exe.eq_ignore_ascii_case(&cur.exe) => cur = p,
                _ => break,
            }
        }
        cur.pid
    }
}

/// Picks the single pid whose tree to exclude for "everything except these processes".
///
/// OS process loopback can exclude exactly one tree. The caller passes e.g.
/// `[electron_main, engine]` where the engine is a child of Electron's main process, so the
/// first pid's tree already covers everything. More generally this returns the pid whose tree
/// contains the most of the others (ties: the earliest in the list), so the order of `pids`
/// does not matter when one of them is an ancestor of the rest.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub fn exclusion_root(pids: &[u32], table: &ProcTable) -> Option<u32> {
    let mut best: Option<(usize, u32)> = None;
    for &cand in pids {
        let covered = pids.iter().filter(|&&p| table.in_tree(p, cand)).count();
        if best.is_none_or(|(n, _)| covered > n) {
            best = Some((covered, cand));
        }
    }
    best.map(|(_, pid)| pid)
}

/// Parses an Electron `desktopCapturer` source id of the form `window:<id>:<n>` and returns
/// `<id>` (an HWND value on Windows, an X11 window id on Linux). `screen:` ids return `None`.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub fn parse_window_source_id(source_id: &str) -> Option<u64> {
    let rest = source_id.strip_prefix("window:")?;
    let id = rest.split(':').next()?;
    let v: u64 = id.parse().ok()?;
    (v != 0).then_some(v)
}

/// Friendly fallback name from an executable path or name: the file stem with a leading
/// capital (`"C:\\x\\firefox.exe"` -> `"Firefox"`, `"msedge"` -> `"Msedge"`).
pub fn title_from_exe(exe: &str) -> String {
    let file = exe.rsplit(['/', '\\']).next().unwrap_or(exe);
    let stem = match file.rfind('.') {
        Some(i) if i > 0 => &file[..i],
        _ => file,
    };
    let mut chars = stem.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Parses `/proc/<pid>/stat` content into `(pid, comm, ppid)`. `comm` may itself contain
/// spaces and parentheses, so the split is on the *last* `)`.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub fn parse_proc_stat(stat: &str) -> Option<(u32, String, u32)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    if close < open {
        return None;
    }
    let pid = stat[..open].trim().parse().ok()?;
    let comm = stat[open + 1..close].to_string();
    let mut rest = stat[close + 1..].split_whitespace();
    let _state = rest.next()?;
    let ppid = rest.next()?.parse().ok()?;
    Some((pid, comm, ppid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> ProcTable {
        // 1 explorer -> 10 chatter(electron) -> 11 renderer, 12 engine -> 13 helper
        // 1 -> 20 chrome -> 21 chrome (audio) ; 1 -> 30 game -> 31 crashpad
        // 40 orphan whose parent 99 is gone; 50 <-> 51 reused-pid cycle
        let e = |pid, ppid, exe: &str| ProcEntry {
            pid,
            ppid,
            exe: exe.into(),
        };
        ProcTable::new([
            e(1, 0, "explorer.exe"),
            e(10, 1, "Chatter.exe"),
            e(11, 10, "Chatter.exe"),
            e(12, 10, "chatter-engine.exe"),
            e(13, 12, "helper.exe"),
            e(20, 1, "chrome.exe"),
            e(21, 20, "CHROME.EXE"),
            e(30, 1, "game.exe"),
            e(31, 30, "crashpad.exe"),
            e(40, 99, "orphan.exe"),
            e(50, 51, "a.exe"),
            e(51, 50, "a.exe"),
        ])
    }

    #[test]
    fn trees_include_descendants() {
        let t = table();
        let mut v: Vec<u32> = t.trees(&[10]).into_iter().collect();
        v.sort();
        assert_eq!(v, vec![10, 11, 12, 13]);
        let mut v: Vec<u32> = t.trees(&[12, 30, 777]).into_iter().collect();
        v.sort();
        assert_eq!(v, vec![12, 13, 30, 31, 777]);
        // Cycles terminate.
        assert_eq!(t.trees(&[50]).len(), 2);
    }

    #[test]
    fn in_tree_walks_up() {
        let t = table();
        assert!(t.in_tree(13, 10));
        assert!(t.in_tree(10, 10));
        assert!(!t.in_tree(20, 10));
        assert!(!t.in_tree(40, 10));
        assert!(!t.in_tree(50, 1)); // cycle terminates
    }

    #[test]
    fn app_root_collapses_same_exe() {
        let t = table();
        assert_eq!(t.app_root(21), 20);
        assert_eq!(t.app_root(11), 10);
        assert_eq!(t.app_root(12), 12);
        assert_eq!(t.app_root(31), 31);
        assert_eq!(t.app_root(12345), 12345);
        let r = t.app_root(50);
        assert!(r == 50 || r == 51);
    }

    #[test]
    fn exclusion_root_prefers_ancestor() {
        let t = table();
        assert_eq!(exclusion_root(&[10, 12], &t), Some(10));
        assert_eq!(exclusion_root(&[12, 10], &t), Some(10));
        assert_eq!(exclusion_root(&[30, 20], &t), Some(30));
        assert_eq!(exclusion_root(&[], &t), None);
        assert_eq!(exclusion_root(&[999], &t), Some(999));
    }

    #[test]
    fn window_source_ids() {
        assert_eq!(parse_window_source_id("window:132456:0"), Some(132_456));
        assert_eq!(parse_window_source_id("window:42"), Some(42));
        assert_eq!(parse_window_source_id("screen:0:0"), None);
        assert_eq!(parse_window_source_id("window:abc:0"), None);
        assert_eq!(parse_window_source_id("window:0:0"), None);
        assert_eq!(parse_window_source_id(""), None);
    }

    #[test]
    fn exe_titles() {
        assert_eq!(
            title_from_exe(r"C:\Program Files\Mozilla\firefox.exe"),
            "Firefox"
        );
        assert_eq!(title_from_exe("/usr/lib/spotify/spotify"), "Spotify");
        assert_eq!(title_from_exe("vlc"), "Vlc");
        assert_eq!(title_from_exe(".hidden"), ".hidden");
        assert_eq!(title_from_exe(""), "");
    }

    #[test]
    fn proc_stat_parsing() {
        let s = "1234 (Web Content (x)) S 1200 1234 1200 0 -1 4194560 3\n";
        assert_eq!(
            parse_proc_stat(s),
            Some((1234, "Web Content (x)".into(), 1200))
        );
        assert_eq!(
            parse_proc_stat("1 (init) S 0 1 1"),
            Some((1, "init".into(), 0))
        );
        assert_eq!(parse_proc_stat("garbage"), None);
    }
}
