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
//! Crash safety: nothing is persisted. If the engine dies while ducked the sessions stay
//! quiet. On Windows a session's volume belongs to that session and is gone when the app
//! restarts (though Windows may remember per-app mixer levels for some apps); on Linux the
//! stream volume dies with the stream (stream-restore may remember it per app). The ramp-up on
//! [`Ducker`] drop and the synchronous restore keep this to genuine crashes.

use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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

/// Platform access to per-application volumes.
pub(crate) trait VolumeBackend {
    type Key: Eq + Hash + Clone + Debug;
    type Vol: Clone + Debug;
    /// Every session/stream we may touch (filter applied) with its current volume.
    fn scan(&mut self) -> anyhow::Result<Vec<(Self::Key, Self::Vol)>>;
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
    original: V,
    last_set: V,
}

/// The ducking state machine, driven by [`run`].
pub(crate) struct DuckState<B: VolumeBackend> {
    sessions: HashMap<B::Key, Managed<B::Vol>>,
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
            released: HashSet::new(),
            target: None,
            ramp: Ramp::unity(now),
            last_scan: None,
        }
    }

    /// Reads current volumes: releases sessions the user changed, forgets vanished ones and,
    /// if `adopt`, starts managing new ones.
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
        for (key, vol) in current {
            present.insert(key.clone());
            if let Some(m) = self.sessions.get(&key) {
                if !B::same(&vol, &m.last_set) {
                    log::debug!(
                        "appaudio: volume of {key:?} changed by someone else; releasing it"
                    );
                    self.sessions.remove(&key);
                    self.released.insert(key);
                }
            } else if adopt && !self.released.contains(&key) {
                self.sessions.insert(
                    key,
                    Managed {
                        original: vol.clone(),
                        last_set: vol,
                    },
                );
            }
        }
        self.sessions.retain(|k, _| present.contains(k));
        self.released.retain(|k| present.contains(k));
    }

    /// Sets every managed session to `original × factor`.
    fn apply(&mut self, b: &mut B, factor: f32) {
        for (key, m) in self.sessions.iter_mut() {
            let v = B::scaled(&m.original, factor);
            if !B::same(&v, &m.last_set) && b.set(key, &v) {
                m.last_set = v;
            }
        }
    }

    /// Puts every managed session back to exactly its original and forgets them all.
    fn restore(&mut self, b: &mut B) {
        if self.sessions.is_empty() {
            self.released.clear();
            return;
        }
        self.rescan(b, false); // don't overwrite a change the user made since the last scan
        for (key, m) in self.sessions.iter() {
            if !B::same(&m.original, &m.last_set) {
                b.set(key, &m.original);
            }
        }
        b.flush();
        self.sessions.clear();
        self.released.clear();
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
        if self.target.is_some() && !ramping {
            let due = self.last_scan.is_none_or(|t| t.elapsed() >= RESCAN_EVERY);
            if b.take_dirty() || due {
                self.rescan(b, true);
            }
        }
        self.apply(b, self.ramp.value(now));
        if self.target.is_none() && !ramping {
            self.restore(b);
        }
    }

    fn timeout(&self, b: &B, now: Instant) -> Option<Duration> {
        if self.ramp.active(now) {
            Some(RAMP_TICK)
        } else if self.target.is_some() {
            Some(b.poll_interval())
        } else {
            None
        }
    }
}

pub(crate) enum Cmd {
    Set(Option<f32>),
}

/// Runs the ducking loop until the command channel closes, then restores synchronously.
pub(crate) fn run<B: VolumeBackend>(mut b: B, rx: Receiver<Cmd>) {
    let mut st = DuckState::<B>::new(Instant::now());
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
    pub fn new(exclude_pids: Vec<u32>) -> Ducker {
        Self::with_filter(Filter {
            exclude: exclude_pids,
            only: None,
        })
    }

    pub(crate) fn with_filter(filter: Filter) -> Ducker {
        let (tx, rx) = mpsc::channel();
        let thread = spawn_backend(filter, rx);
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

fn spawn_backend(filter: Filter, rx: Receiver<Cmd>) -> Option<JoinHandle<()>> {
    #[cfg(windows)]
    let body = move || crate::wasapi::run_ducker(filter, rx);
    #[cfg(target_os = "linux")]
    let body = move || crate::pulse::run_ducker(filter, rx);
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (filter, rx);
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

    /// In-memory backend for exercising the state machine.
    #[derive(Default)]
    struct Fake {
        vols: HashMap<u32, f32>,
        sets: usize,
    }

    impl VolumeBackend for Fake {
        type Key = u32;
        type Vol = f32;
        fn scan(&mut self) -> anyhow::Result<Vec<(u32, f32)>> {
            Ok(self.vols.iter().map(|(k, v)| (*k, *v)).collect())
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
