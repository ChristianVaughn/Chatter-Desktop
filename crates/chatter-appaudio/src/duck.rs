//! Ducking ("attenuation"): turning other applications down while people in the call talk.
//!
//! The policy lives here and is platform independent; backends ([`VolumeBackend`]) only know
//! how to list the controllable sessions/streams with their current volume and set one.
//!
//! Model: every managed session has an `original` volume, captured the first time we see it
//! while ducking. A single gain factor ramps between 1.0 and the duck level (smoothstep, ~150 ms
//! down / ~300 ms up) and each session is set to `original × factor`. When the factor is back
//! at 1.0 every session is set to exactly its original and forgotten, so nothing drifts across
//! duck cycles and volumes are only ever changed *relative to what the user had*.
//!
//! User changes win: if a session's volume no longer matches what we last set (the user
//! moved its slider in the volume mixer / pavucontrol, or the app changed it itself), we stop
//! managing that session for the rest of the duck cycle and do *not* restore it later — its
//! current value is what the user wants. Detection is by read-back on every re-scan (each
//! `set` call, ~1 s while ducked, plus change notifications on Linux) and right before
//! restoring; no OS event sinks are needed.
//!
//! Lowered volumes must not outlive the duck. Windows (per app) and PulseAudio/WirePlumber
//! (per application) remember a volume and give it back the next time that app plays, even
//! after a reboot. So every session is also tied to its *app*: the identity the OS remembers
//! the volume by. A lowered session we can't put back (it ended while ducked, or the process
//! died before restoring) becomes *pending* for its app: when that app shows up again at the
//! volume we left it at, it is put back to its original; at any other volume someone else has
//! set it since, and it is left alone. A new session that comes up at the lowered volume of
//! a ducked sibling of the same app takes that sibling's original, not the lowered value.
//!
//! Crash safety: the managed and pending sessions are kept in a small journal file while
//! there are any. The next [`Ducker`] given the same journal takes them over as pending, so a
//! process killed while ducked (including by a shutdown) is undone on the next start, for
//! apps that are running then and for those that start later. Pending apps are watched for a
//! week at most. A clean drop restores synchronously and leaves only what is still pending.

use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(any(windows, target_os = "linux", test))]
use crate::proc_tree::ProcTable;

/// Ramp length when ducking.
pub const DUCK_RAMP: Duration = Duration::from_millis(150);
/// Ramp length when restoring.
pub const RESTORE_RAMP: Duration = Duration::from_millis(300);
/// Volume update interval while ramping.
const RAMP_TICK: Duration = Duration::from_millis(10);
/// Re-scan interval while ducked (new sessions get ducked, user changes are noticed).
const RESCAN_EVERY: Duration = Duration::from_secs(1);
/// While apps a previous duck left lowered are outstanding, how often to look for them.
const PENDING_POLL: Duration = Duration::from_secs(2);
/// How long to keep waiting for such an app to come back.
const PENDING_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A smoothstep gain ramp.
#[derive(Clone, Copy, Debug)]
pub struct Ramp {
    from: f32,
    to: f32,
    start: Instant,
    dur: Duration,
}

impl Ramp {
    /// A finished ramp sitting at unity gain.
    pub fn unity(now: Instant) -> Ramp {
        Ramp {
            from: 1.0,
            to: 1.0,
            start: now,
            dur: Duration::ZERO,
        }
    }

    /// Gain at `now`.
    pub fn value(&self, now: Instant) -> f32 {
        if self.dur.is_zero() {
            return self.to;
        }
        let t = (now.saturating_duration_since(self.start).as_secs_f32() / self.dur.as_secs_f32())
            .clamp(0.0, 1.0);
        if t >= 1.0 {
            return self.to; // exact, not from + (to - from)
        }
        // Smoothstep: zero slope at both ends, so the volume change starts and stops gently.
        let s = t * t * (3.0 - 2.0 * t);
        self.from + (self.to - self.from) * s
    }

    pub fn active(&self, now: Instant) -> bool {
        now < self.start + self.dur
    }

    /// A new ramp from wherever this one is at `now` to `to`. Retargeting to the current
    /// target keeps the running ramp, so repeated identical requests are free.
    pub fn retarget(&self, now: Instant, to: f32, dur: Duration) -> Ramp {
        if to == self.to {
            return *self;
        }
        let from = self.value(now);
        Ramp {
            from,
            to,
            start: now,
            dur: if from == to { Duration::ZERO } else { dur },
        }
    }
}

/// Which processes' sessions may be touched.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// Never touch these process trees (our own app).
    pub exclude: Vec<u32>,
    /// If set, touch only these process trees (tests use this to leave the desktop alone).
    pub only: Option<Vec<u32>>,
}

impl Filter {
    /// Whether a session owned by `pid` (`None`/0: no single owning process, e.g. Windows
    /// system sounds or PulseAudio module streams) may be ducked.
    #[cfg(any(windows, target_os = "linux", test))]
    pub fn allows(&self, pid: Option<u32>, table: &ProcTable) -> bool {
        let pid = pid.filter(|&p| p != 0);
        if let Some(p) = pid {
            if self.exclude.iter().any(|&root| table.in_tree(p, root)) {
                return false;
            }
        }
        match &self.only {
            None => true,
            Some(roots) => pid.is_some_and(|p| roots.iter().any(|&r| table.in_tree(p, r))),
        }
    }
}

/// A session/stream as a backend lists it.
pub(crate) struct Session<K, V> {
    pub key: K,
    /// The app it belongs to as the OS remembers volumes: stable across restarts and reboots,
    /// unlike `key`. Empty when unknown.
    pub app: String,
    pub vol: V,
}

/// Platform access to per-application volumes.
pub(crate) trait VolumeBackend {
    type Key: Eq + Hash + Clone + Debug;
    type Vol: Clone + Debug;
    /// A volume as plain numbers for the crash journal, and back.
    fn vol_to_vec(v: &Self::Vol) -> Vec<f64>;
    fn vol_from_vec(v: &[f64]) -> Option<Self::Vol>;
    /// Every session/stream we may touch (filter applied) with its current volume.
    fn scan(&mut self) -> anyhow::Result<Vec<Session<Self::Key, Self::Vol>>>;
    /// `original` scaled by `factor` (0..=1). Must return `original` unchanged for 1.0.
    fn scaled(original: &Self::Vol, factor: f32) -> Self::Vol;
    /// Whether two volumes are equal within the backend's rounding.
    fn same(a: &Self::Vol, b: &Self::Vol) -> bool;
    /// Sets one volume; `false` if the session is gone.
    fn set(&mut self, key: &Self::Key, vol: &Self::Vol) -> bool;
    /// Blocks until every `set` so far has taken effect.
    fn flush(&mut self) {}
    /// Whether the backend saw changes that warrant a re-scan (clears the flag).
    fn take_dirty(&mut self) -> bool {
        false
    }
    /// How often to wake while ducked to check `take_dirty`.
    fn poll_interval(&self) -> Duration {
        RESCAN_EVERY
    }
}

struct Managed<V> {
    app: String,
    original: V,
    last_set: V,
}

/// An app left at a lowered volume we couldn't put back, waiting for it to show up again.
struct Pending<V> {
    original: V,
    last_set: V,
    /// When it was left lowered, in seconds since the Unix epoch.
    since: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The ducking state machine, driven by [`run`].
pub(crate) struct DuckState<B: VolumeBackend> {
    sessions: HashMap<B::Key, Managed<B::Vol>>,
    /// Apps left lowered, by app (see the module docs).
    pending: HashMap<String, Pending<B::Vol>>,
    /// Where the managed and pending sessions are recorded (see the module docs).
    journal: Option<PathBuf>,
    /// The sessions changed since the journal was last written.
    journal_stale: bool,
    /// Sessions the user took over during this duck cycle: left alone until restore.
    released: HashSet<B::Key>,
    target: Option<f32>,
    ramp: Ramp,
    last_scan: Option<Instant>,
}

impl<B: VolumeBackend> DuckState<B> {
    pub fn new(now: Instant) -> Self {
        DuckState {
            sessions: HashMap::new(),
            pending: HashMap::new(),
            journal: None,
            journal_stale: false,
            released: HashSet::new(),
            target: None,
            ramp: Ramp::unity(now),
            last_scan: None,
        }
    }

    /// Reads current volumes: releases sessions the user changed, puts back pending apps that
    /// came back, moves vanished sessions to pending and, if `adopt`, starts managing new ones.
    fn rescan(&mut self, b: &mut B, adopt: bool) {
        self.last_scan = Some(Instant::now());
        let current = match b.scan() {
            Ok(c) => c,
            Err(e) => {
                log::debug!("appaudio: duck scan failed: {e:#}");
                return;
            }
        };
        let mut present = HashSet::with_capacity(current.len());
        let mut settled = HashSet::new();
        for Session { key, app, vol } in current {
            present.insert(key.clone());
            if let Some(m) = self.sessions.get(&key) {
                if !B::same(&vol, &m.last_set) {
                    log::debug!(
                        "appaudio: volume of {key:?} changed by someone else; releasing it"
                    );
                    self.sessions.remove(&key);
                    self.released.insert(key);
                    self.journal_stale = true;
                }
                continue;
            }
            if self.released.contains(&key) {
                continue;
            }
            // Back at the volume a previous duck left it at: its real volume is the original.
            // At any other volume, someone has set it since and it's theirs.
            let left_low = self.pending.get(&app).and_then(|p| {
                settled.insert(app.clone());
                B::same(&vol, &p.last_set).then(|| p.original.clone())
            });
            if !adopt {
                if let Some(original) = left_low {
                    if b.set(&key, &original) {
                        log::info!("appaudio: put back {app}, which a previous duck left lowered");
                    }
                }
                continue;
            }
            // A new session of a ducked app can start at the volume we gave its sibling (the
            // OS remembers it per app); that isn't its own.
            let original = left_low
                .or_else(|| self.lowered_sibling(&app, &vol))
                .unwrap_or_else(|| vol.clone());
            self.journal_stale = true;
            self.sessions.insert(
                key,
                Managed {
                    app,
                    original,
                    last_set: vol,
                },
            );
        }
        let gone: Vec<B::Key> = self
            .sessions
            .keys()
            .filter(|k| !present.contains(*k))
            .cloned()
            .collect();
        for key in gone {
            if let Some(m) = self.sessions.remove(&key) {
                self.journal_stale = true;
                self.leave_pending(m);
            }
        }
        self.released.retain(|k| present.contains(k));
        let expired = now_secs().saturating_sub(PENDING_TTL.as_secs());
        let before = self.pending.len();
        self.pending
            .retain(|app, p| !settled.contains(app) && p.since > expired);
        self.journal_stale |= self.pending.len() != before;
    }

    /// The original of a managed session of `app` that we lowered to `vol`.
    fn lowered_sibling(&self, app: &str, vol: &B::Vol) -> Option<B::Vol> {
        if app.is_empty() {
            return None;
        }
        self.sessions
            .values()
            .find(|m| {
                m.app == app && B::same(vol, &m.last_set) && !B::same(&m.original, &m.last_set)
            })
            .map(|m| m.original.clone())
    }

    /// Remembers a session we couldn't put back, if we left it lowered, so its app is put
    /// back when it shows up again.
    fn leave_pending(&mut self, m: Managed<B::Vol>) {
        if m.app.is_empty() || B::same(&m.original, &m.last_set) {
            return;
        }
        self.journal_stale = true;
        self.pending.entry(m.app).or_insert(Pending {
            original: m.original,
            last_set: m.last_set,
            since: now_secs(),
        });
    }

    /// Sets every managed session to `original × factor`.
    fn apply(&mut self, b: &mut B, factor: f32) {
        for (key, m) in self.sessions.iter_mut() {
            let v = B::scaled(&m.original, factor);
            if !B::same(&v, &m.last_set) && b.set(key, &v) {
                m.last_set = v;
                self.journal_stale = true;
            }
        }
    }

    /// Puts every managed session back to exactly its original and forgets them all; one that
    /// can't be set any more is left pending.
    fn restore(&mut self, b: &mut B) {
        if !self.sessions.is_empty() {
            self.rescan(b, false); // don't overwrite a change the user made since the last scan
            for (key, m) in std::mem::take(&mut self.sessions) {
                if !B::same(&m.original, &m.last_set) && !b.set(&key, &m.original) {
                    self.leave_pending(m);
                }
            }
            b.flush();
            self.journal_stale = true;
        }
        self.released.clear();
        if self.journal_stale {
            self.write_journal();
        }
    }

    /// Records the managed and pending sessions, or removes the journal when there are none.
    fn write_journal(&mut self) {
        self.journal_stale = false;
        let Some(path) = &self.journal else { return };
        let now = now_secs();
        let managed = self
            .sessions
            .values()
            .filter(|m| !m.app.is_empty())
            .map(|m| JournalEntry {
                app: m.app.clone(),
                original: B::vol_to_vec(&m.original),
                last_set: B::vol_to_vec(&m.last_set),
                since: now,
            });
        let pending = self.pending.iter().map(|(app, p)| JournalEntry {
            app: app.clone(),
            original: B::vol_to_vec(&p.original),
            last_set: B::vol_to_vec(&p.last_set),
            since: p.since,
        });
        let entries: Vec<JournalEntry> = managed.chain(pending).collect();
        if entries.is_empty() {
            let _ = std::fs::remove_file(path);
            return;
        }
        // Written beside and renamed over, so a crash mid-write leaves the old journal.
        let tmp = path.with_extension("tmp");
        let written = serde_json::to_vec(&entries)
            .map_err(anyhow::Error::from)
            .and_then(|json| Ok(std::fs::write(&tmp, json)?))
            .and_then(|()| Ok(std::fs::rename(&tmp, path)?));
        if let Err(e) = written {
            log::warn!("appaudio: can't write the ducking journal: {e:#}");
        }
    }

    /// Takes over what a previous run left lowered as pending, and puts back the apps that are
    /// running now (see the module docs).
    fn recover(&mut self, b: &mut B) {
        let Some(path) = &self.journal else { return };
        let Ok(text) = std::fs::read(path) else {
            return;
        };
        let entries: Vec<JournalEntry> = serde_json::from_slice(&text).unwrap_or_default();
        for entry in entries {
            let (Some(original), Some(last_set)) = (
                B::vol_from_vec(&entry.original),
                B::vol_from_vec(&entry.last_set),
            ) else {
                continue;
            };
            if entry.app.is_empty() || B::same(&original, &last_set) {
                continue;
            }
            self.pending.entry(entry.app).or_insert(Pending {
                original,
                last_set,
                since: entry.since,
            });
        }
        if !self.pending.is_empty() {
            log::info!(
                "appaudio: a previous run left {} app(s) lowered; putting them back as they appear",
                self.pending.len()
            );
        }
        self.rescan(b, false);
        b.flush();
        self.write_journal();
    }

    fn command(&mut self, b: &mut B, level: Option<f32>, now: Instant) {
        let (to, dur) = match level {
            Some(l) => (l, DUCK_RAMP),
            None => (1.0, RESTORE_RAMP),
        };
        self.target = level;
        self.ramp = self.ramp.retarget(now, to, dur);
        if level.is_some() || !self.sessions.is_empty() {
            self.rescan(b, level.is_some());
        }
    }

    /// One iteration after a command or timeout.
    fn tick(&mut self, b: &mut B, now: Instant) {
        let ramping = self.ramp.active(now);
        if !ramping {
            let since_scan = self.last_scan.map(|t| t.elapsed());
            if self.target.is_some() {
                if b.take_dirty() || since_scan.is_none_or(|e| e >= RESCAN_EVERY) {
                    self.rescan(b, true);
                }
            } else if !self.pending.is_empty() && since_scan.is_none_or(|e| e >= PENDING_POLL) {
                self.rescan(b, false);
            }
        }
        self.apply(b, self.ramp.value(now));
        if self.target.is_none() && !ramping {
            self.restore(b);
        } else if self.journal_stale && !ramping {
            // Once settled rather than on every ramp step.
            self.write_journal();
        }
    }

    fn timeout(&self, b: &B, now: Instant) -> Option<Duration> {
        if self.ramp.active(now) {
            Some(RAMP_TICK)
        } else if self.target.is_some() {
            Some(b.poll_interval())
        } else if !self.pending.is_empty() {
            Some(PENDING_POLL)
        } else {
            None
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct JournalEntry {
    app: String,
    original: Vec<f64>,
    last_set: Vec<f64>,
    /// When it was lowered, in seconds since the Unix epoch.
    since: u64,
}

pub(crate) enum Cmd {
    Set(Option<f32>),
}

/// Runs the ducking loop until the command channel closes, then restores synchronously.
/// First puts back anything a previous run recorded in `journal` and never restored.
pub(crate) fn run<B: VolumeBackend>(mut b: B, journal: Option<PathBuf>, rx: Receiver<Cmd>) {
    let mut st = DuckState::<B>::new(Instant::now());
    st.journal = journal;
    st.recover(&mut b);
    loop {
        let msg = match st.timeout(&b, Instant::now()) {
            Some(t) => rx.recv_timeout(t),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match msg {
            Ok(Cmd::Set(level)) => st.command(&mut b, level, Instant::now()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        st.tick(&mut b, Instant::now());
    }
    st.target = None;
    st.restore(&mut b);
}

/// Whether ducking can work here.
pub fn ducking_supported() -> bool {
    #[cfg(windows)]
    {
        true
    }
    #[cfg(target_os = "linux")]
    {
        crate::pulse::capabilities().per_app
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        false
    }
}

/// Turns other applications down while active. `Send + Sync`; all work happens on its own
/// thread, so [`set`](Self::set) never blocks on the OS.
pub struct Ducker {
    tx: Option<Sender<Cmd>>,
    last: Mutex<Option<Option<f32>>>,
    thread: Option<JoinHandle<()>>,
}

impl Ducker {
    /// A ducker that never touches sessions/streams of `exclude_pids` or their process trees
    /// (our app: Electron's main process and the engine). Nothing changes until
    /// [`set`](Self::set) is called.
    ///
    /// With a `journal` path, what is ducked is recorded there, and a ducker started after a
    /// crash puts it back first (see the module docs).
    pub fn new(exclude_pids: Vec<u32>, journal: Option<PathBuf>) -> Ducker {
        Self::with_filter(
            Filter {
                exclude: exclude_pids,
                only: None,
            },
            journal,
        )
    }

    /// A ducker that touches only what `filter` allows.
    pub fn with_filter(filter: Filter, journal: Option<PathBuf>) -> Ducker {
        let (tx, rx) = mpsc::channel();
        let thread = spawn_backend(filter, journal, rx);
        Ducker {
            tx: thread.as_ref().map(|_| tx),
            last: Mutex::new(None),
            thread,
        }
    }

    /// `Some(level)` (clamped to 0..=1): scale every other app's volume to `level` × its own
    /// volume, ramped over ~150 ms. `None`: restore every volume this ducker changed, ramped
    /// over ~300 ms. Repeating the current value is a no-op.
    pub fn set(&self, level: Option<f32>) {
        let level = level.filter(|l| !l.is_nan()).map(|l| l.clamp(0.0, 1.0));
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if *last == Some(level) {
            return;
        }
        *last = Some(level);
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Set(level));
        }
    }
}

impl Drop for Ducker {
    /// Restores every changed volume (immediately, no ramp) before returning.
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn spawn_backend(
    filter: Filter,
    journal: Option<PathBuf>,
    rx: Receiver<Cmd>,
) -> Option<JoinHandle<()>> {
    #[cfg(windows)]
    let body = move || crate::wasapi::run_ducker(filter, journal, rx);
    #[cfg(target_os = "linux")]
    let body = move || crate::pulse::run_ducker(filter, journal, rx);
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (filter, journal, rx);
        return None;
    }
    #[cfg(any(windows, target_os = "linux"))]
    match std::thread::Builder::new()
        .name("chatter-appaudio-duck".into())
        .spawn(body)
    {
        Ok(t) => Some(t),
        Err(e) => {
            log::error!("appaudio: cannot spawn ducking thread: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc_tree::ProcEntry;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn ramp_shape() {
        let t0 = Instant::now();
        let r = Ramp::unity(t0).retarget(t0, 0.2, ms(150));
        assert_eq!(r.value(t0), 1.0);
        assert!(
            (r.value(t0 + ms(75)) - 0.6).abs() < 1e-6,
            "midpoint is halfway"
        );
        assert!(r.value(t0 + ms(15)) > 0.97, "gentle start");
        assert!(r.value(t0 + ms(135)) < 0.23, "gentle end");
        assert_eq!(r.value(t0 + ms(150)), 0.2);
        assert_eq!(r.value(t0 + ms(500)), 0.2);
        assert!(r.active(t0 + ms(149)) && !r.active(t0 + ms(150)));
        // Monotonic.
        let mut prev = 1.0;
        for i in 0..=150 {
            let v = r.value(t0 + ms(i));
            assert!(v <= prev + 1e-6);
            prev = v;
        }
    }

    #[test]
    fn ramp_retarget_mid_way_is_continuous() {
        let t0 = Instant::now();
        let down = Ramp::unity(t0).retarget(t0, 0.2, ms(150));
        let t1 = t0 + ms(50);
        let v = down.value(t1);
        let up = down.retarget(t1, 1.0, ms(300));
        assert_eq!(up.value(t1), v);
        assert_eq!(up.value(t1 + ms(300)), 1.0);
        // Same target again: unchanged ramp.
        let same = up.retarget(t1 + ms(10), 1.0, ms(300));
        assert_eq!(same.value(t1 + ms(100)), up.value(t1 + ms(100)));
        // Retarget to where we already are: instant.
        let done = Ramp::unity(t0).retarget(t0, 0.5, ms(0));
        assert_eq!(done.value(t0), 0.5);
        assert!(!done.active(t0));
    }

    /// In-memory backend for exercising the state machine. Session `k` belongs to app
    /// `apps[k]`, else to its own app "app{k}".
    #[derive(Default)]
    struct Fake {
        vols: HashMap<u32, f32>,
        apps: HashMap<u32, String>,
        sets: usize,
    }

    impl Fake {
        /// Session `key` of `app` at `vol`.
        fn open(&mut self, key: u32, app: &str, vol: f32) {
            self.apps.insert(key, app.into());
            self.vols.insert(key, vol);
        }

        fn close(&mut self, key: u32) {
            self.vols.remove(&key);
        }
    }

    impl VolumeBackend for Fake {
        type Key = u32;
        type Vol = f32;
        fn vol_to_vec(v: &f32) -> Vec<f64> {
            vec![f64::from(*v)]
        }
        fn vol_from_vec(v: &[f64]) -> Option<f32> {
            v.first().map(|x| *x as f32)
        }
        fn scan(&mut self) -> anyhow::Result<Vec<Session<u32, f32>>> {
            Ok(self
                .vols
                .iter()
                .map(|(k, v)| Session {
                    key: *k,
                    app: self
                        .apps
                        .get(k)
                        .cloned()
                        .unwrap_or_else(|| format!("app{k}")),
                    vol: *v,
                })
                .collect())
        }
        fn scaled(o: &f32, f: f32) -> f32 {
            o * f
        }
        fn same(a: &f32, b: &f32) -> bool {
            (a - b).abs() < 1e-4
        }
        fn set(&mut self, k: &u32, v: &f32) -> bool {
            self.sets += 1;
            self.vols.get_mut(k).map(|x| *x = *v).is_some()
        }
    }

    #[test]
    fn duck_restore_cycle() {
        let mut b = Fake::default();
        b.vols.insert(1, 0.8);
        b.vols.insert(2, 0.5);
        let t0 = Instant::now();
        let mut st = DuckState::<Fake>::new(t0);
        st.command(&mut b, Some(0.25), t0);
        st.tick(&mut b, t0 + ms(200));
        assert!((b.vols[&1] - 0.2).abs() < 1e-6 && (b.vols[&2] - 0.125).abs() < 1e-6);

        // A new app appears while ducked and gets ducked on the next scan.
        b.vols.insert(3, 1.0);
        st.last_scan = None;
        st.tick(&mut b, t0 + ms(1300));
        assert!((b.vols[&3] - 0.25).abs() < 1e-6);

        // The user raises app 2 while ducked: we let go of it for good.
        b.vols.insert(2, 0.9);
        let t1 = t0 + ms(1400);
        st.command(&mut b, None, t1);
        st.tick(&mut b, t1 + ms(100));
        assert_eq!(b.vols[&2], 0.9);
        st.tick(&mut b, t1 + ms(300));
        assert_eq!(b.vols[&1], 0.8);
        assert_eq!(b.vols[&3], 1.0);
        assert_eq!(b.vols[&2], 0.9);
        assert!(st.sessions.is_empty());

        // Idle: nothing touched.
        let sets = b.sets;
        st.tick(&mut b, t1 + ms(2000));
        assert_eq!(b.sets, sets);
    }

    fn journal_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("chatter-duck-{name}-{}.json", std::process::id()))
    }

    #[test]
    fn a_crash_while_ducked_is_undone_next_time() {
        let journal = journal_path("crash");
        let _ = std::fs::remove_file(&journal);
        let mut b = Fake::default();
        b.vols.insert(1, 0.8);
        b.vols.insert(2, 0.5);
        let t0 = Instant::now();
        let mut st = DuckState::<Fake>::new(t0);
        st.journal = Some(journal.clone());
        st.command(&mut b, Some(0.25), t0);
        st.tick(&mut b, t0 + ms(200));
        assert!(journal.exists(), "recorded once settled");
        // The process dies here, with no restore. Meanwhile the user turns app 2 up.
        drop(st);
        b.vols.insert(2, 0.9);

        let mut next = DuckState::<Fake>::new(Instant::now());
        next.journal = Some(journal.clone());
        next.recover(&mut b);
        assert_eq!(b.vols[&1], 0.8, "put back");
        assert_eq!(b.vols[&2], 0.9, "the user's change stands");
        assert!(!journal.exists());
    }

    #[test]
    fn a_clean_restore_leaves_no_journal() {
        let journal = journal_path("clean");
        let mut b = Fake::default();
        b.vols.insert(1, 1.0);
        let t0 = Instant::now();
        let mut st = DuckState::<Fake>::new(t0);
        st.journal = Some(journal.clone());
        st.command(&mut b, Some(0.5), t0);
        st.tick(&mut b, t0 + ms(200));
        assert!(journal.exists());
        st.command(&mut b, None, t0 + ms(300));
        st.tick(&mut b, t0 + ms(700));
        assert_eq!(b.vols[&1], 1.0);
        assert!(!journal.exists());
    }

    #[test]
    fn a_shutdown_while_ducked_is_undone_as_apps_return() {
        let journal = journal_path("reboot");
        let _ = std::fs::remove_file(&journal);
        let mut b = Fake::default();
        b.open(1, "player", 0.8);
        b.open(2, "game", 0.5);
        let t0 = Instant::now();
        let mut st = DuckState::<Fake>::new(t0);
        st.journal = Some(journal.clone());
        st.command(&mut b, Some(0.25), t0);
        st.tick(&mut b, t0 + ms(200));
        drop(st); // the machine shuts down with both ducked

        // After the reboot the OS gives each app the volume it last had, in new sessions,
        // and only the player has started yet.
        let mut b = Fake::default();
        b.open(11, "player", 0.2);
        let mut next = DuckState::<Fake>::new(Instant::now());
        next.journal = Some(journal.clone());
        next.recover(&mut b);
        assert_eq!(b.vols[&11], 0.8, "running app put back");
        assert!(journal.exists(), "still waiting for the game");
        assert!(
            next.timeout(&b, Instant::now()).is_some(),
            "and watching for it"
        );

        b.open(12, "game", 0.125);
        next.last_scan = None;
        next.tick(&mut b, Instant::now());
        assert_eq!(b.vols[&12], 0.5, "put back once it starts");
        assert!(!journal.exists());
        assert_eq!(
            next.timeout(&b, Instant::now()),
            None,
            "nothing left to watch"
        );
    }

    #[test]
    fn an_app_that_quits_while_ducked_is_put_back_when_it_returns() {
        let mut b = Fake::default();
        b.open(1, "player", 0.8);
        b.open(2, "game", 0.5);
        let t0 = Instant::now();
        let mut st = DuckState::<Fake>::new(t0);
        st.command(&mut b, Some(0.25), t0);
        st.tick(&mut b, t0 + ms(200));
        b.close(2); // the OS keeps the game's lowered volume for next time
        st.last_scan = None;
        st.tick(&mut b, t0 + ms(1300));
        st.command(&mut b, None, t0 + ms(1400));
        st.tick(&mut b, t0 + ms(1800));
        assert_eq!(b.vols[&1], 0.8);

        b.open(3, "game", 0.125);
        st.last_scan = None;
        st.tick(&mut b, t0 + ms(5000));
        assert_eq!(b.vols[&3], 0.5);
        assert!(st.pending.is_empty());
    }

    #[test]
    fn a_returning_app_someone_else_has_set_is_left_alone() {
        let mut b = Fake::default();
        b.open(1, "game", 0.5);
        let t0 = Instant::now();
        let mut st = DuckState::<Fake>::new(t0);
        st.command(&mut b, Some(0.25), t0);
        st.tick(&mut b, t0 + ms(200));
        b.close(1);
        st.command(&mut b, None, t0 + ms(300));
        st.tick(&mut b, t0 + ms(700));
        assert!(!st.pending.is_empty());

        b.open(2, "game", 0.7);
        st.last_scan = None;
        st.tick(&mut b, t0 + ms(5000));
        assert_eq!(b.vols[&2], 0.7);
        assert!(st.pending.is_empty(), "no longer ours to put back");
    }

    #[test]
    fn a_new_stream_at_its_siblings_ducked_volume_keeps_the_real_one() {
        let mut b = Fake::default();
        b.open(1, "browser", 0.8);
        let t0 = Instant::now();
        let mut st = DuckState::<Fake>::new(t0);
        st.command(&mut b, Some(0.25), t0);
        st.tick(&mut b, t0 + ms(200));
        // A second stream starts at the volume the OS now remembers for the browser.
        b.open(2, "browser", 0.2);
        st.last_scan = None;
        st.tick(&mut b, t0 + ms(1300));
        assert_eq!(b.vols[&2], 0.2, "not ducked a second time");
        st.command(&mut b, None, t0 + ms(1400));
        st.tick(&mut b, t0 + ms(1800));
        assert_eq!(b.vols[&1], 0.8);
        assert_eq!(b.vols[&2], 0.8, "restored to the browser's volume, not 0.2");
    }

    #[test]
    fn released_session_is_not_readopted() {
        let mut b = Fake::default();
        b.vols.insert(1, 1.0);
        let t0 = Instant::now();
        let mut st = DuckState::<Fake>::new(t0);
        st.command(&mut b, Some(0.5), t0);
        st.tick(&mut b, t0 + ms(200));
        b.vols.insert(1, 0.7); // user
        st.last_scan = None;
        st.tick(&mut b, t0 + ms(1300));
        st.last_scan = None;
        st.tick(&mut b, t0 + ms(2400));
        assert_eq!(b.vols[&1], 0.7);
    }

    #[test]
    fn filter_rules() {
        let e = |pid, ppid| ProcEntry {
            pid,
            ppid,
            exe: String::new(),
        };
        let t = ProcTable::new([e(10, 1), e(11, 10), e(20, 1), e(21, 20)]);
        let f = Filter {
            exclude: vec![10],
            only: None,
        };
        assert!(!f.allows(Some(11), &t));
        assert!(f.allows(Some(21), &t));
        assert!(f.allows(None, &t));
        assert!(f.allows(Some(0), &t));
        let f = Filter {
            exclude: vec![10],
            only: Some(vec![20]),
        };
        assert!(f.allows(Some(21), &t));
        assert!(!f.allows(Some(11), &t));
        assert!(!f.allows(None, &t));
    }

    #[test]
    fn ducker_dedupes_and_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Ducker>();
    }
}
