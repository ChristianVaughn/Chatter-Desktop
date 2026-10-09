//! Linux backend: PulseAudio client API (works unchanged against `pipewire-pulse`).
//!
//! PulseAudio has no "loopback of a process" stream, but it can record the output of a single
//! *sink input* (one playback stream): connect a record stream to the monitor source of the
//! sink the sink input plays to, after `pa_stream_set_monitor_stream(sink_input_index)`. That
//! is what pavucontrol's per-stream level meters use. So a capture here is:
//!
//! 1. list sink inputs (`application.process.id`) and sinks (for their monitor sources),
//! 2. pick the ones in the target process tree (or outside the excluded trees),
//! 3. open one monitor record stream per sink input, 48 kHz stereo `float32le`,
//! 4. mix them and emit 10 ms chunks on a wall-clock timer (zeros when nothing plays),
//! 5. on sink / sink-input subscription events, repeat 1–3 so streams that apps open, close
//!    or move between sinks are followed.
//!
//! Threading: libpulse's threaded mainloop runs the protocol on its own thread; our capture
//! thread does everything else and only touches PulseAudio objects with the mainloop lock
//! held. Instead of wait/signal handshakes, operations are polled (lock, check, unlock,
//! sleep 1 ms) — they complete within milliseconds and this keeps callbacks trivial: they only
//! write into `Arc<Mutex<_>>`/atomics, never touch PulseAudio objects. Record streams are
//! drained with `peek`/`discard` from the capture thread on every tick, so there are no read
//! callbacks at all.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _};
use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::context::subscribe::InterestMaskSet;
use pulse::context::{Context, FlagSet as ContextFlags, State as ContextState};
use pulse::def::BufferAttr;
use pulse::mainloop::threaded::Mainloop;
use pulse::operation::{Operation, State as OpState};
use pulse::proplist::{properties, Proplist};
use pulse::sample::{Format, Spec};
use pulse::stream::{FlagSet as StreamFlags, PeekResult, State as StreamState, Stream};
use pulse::volume::{ChannelVolumes, Volume, VolumeLinear};

use crate::chunk::{clamp, Ticker, CHUNK_SAMPLES, SAMPLE_RATE};
use crate::duck::{Filter, VolumeBackend};
use crate::proc_tree::{parse_proc_stat, title_from_exe, ProcEntry, ProcTable};
use crate::{AudioApp, Capabilities, Capture, OnAudio, Target};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const OP_TIMEOUT: Duration = Duration::from_secs(3);
/// Minimum spacing of re-listing after subscription events (volume drags fire many).
const REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(100);
/// After losing the server, retry connecting this often (emitting silence meanwhile).
const RECONNECT_EVERY: Duration = Duration::from_secs(1);
/// Per-stream jitter buffer: start consuming a stream once this many samples are buffered...
const PRIME_SAMPLES: usize = CHUNK_SAMPLES * 2;
/// ...and drop the oldest audio beyond this, so latency stays bounded if the server clock
/// runs faster than ours.
const MAX_BUFFER_SAMPLES: usize = CHUNK_SAMPLES * 10;
/// Requested record fragment size: 10 ms of stereo f32.
const FRAGSIZE: u32 = (CHUNK_SAMPLES * 4) as u32;

// ---------------------------------------------------------------------------------------------
// Process table

/// All processes from `/proc/<pid>/stat` (pid, ppid, comm).
fn snapshot() -> ProcTable {
    let mut entries = Vec::new();
    if let Ok(dir) = std::fs::read_dir("/proc") {
        for ent in dir.flatten() {
            let name = ent.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(stat) = std::fs::read_to_string(ent.path().join("stat")) else {
                continue;
            };
            if let Some((pid, comm, ppid)) = parse_proc_stat(&stat) {
                entries.push(ProcEntry {
                    pid,
                    ppid,
                    exe: comm,
                });
            }
        }
    }
    ProcTable::new(entries)
}

// ---------------------------------------------------------------------------------------------
// Connection

/// A threaded mainloop with a connected context.
///
/// Field order matters: the context must be released before the mainloop it was created on.
struct Conn {
    ctx: Context,
    ml: Mainloop,
}

impl Conn {
    fn connect(name: &str) -> anyhow::Result<Conn> {
        let mut ml = Mainloop::new().ok_or_else(|| anyhow!("pa_threaded_mainloop_new failed"))?;
        let mut props = Proplist::new().ok_or_else(|| anyhow!("pa_proplist_new failed"))?;
        let _ = props.set_str(properties::APPLICATION_NAME, "Chatter");
        let _ = props.set_str(properties::APPLICATION_ID, "chat.chatter.desktop");
        // The loop thread is not running yet, so no lock is needed for setup.
        let mut ctx = Context::new_with_proplist(&ml, name, &props)
            .ok_or_else(|| anyhow!("pa_context_new failed"))?;
        ctx.connect(None, ContextFlags::NOAUTOSPAWN, None)
            .map_err(|e| anyhow!("connecting to PulseAudio: {e}"))?;
        ml.start()
            .map_err(|e| anyhow!("starting PulseAudio mainloop: {e}"))?;
        let mut conn = Conn { ctx, ml };

        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            match conn.locked(|c| c.ctx.get_state()) {
                ContextState::Ready => return Ok(conn),
                ContextState::Failed | ContextState::Terminated => {
                    let err = conn.locked(|c| c.ctx.errno());
                    bail!("PulseAudio connection failed: {err}");
                }
                _ if Instant::now() > deadline => bail!("PulseAudio connection timed out"),
                _ => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }

    /// Runs `f` with the mainloop lock held.
    fn locked<R>(&mut self, f: impl FnOnce(&mut Conn) -> R) -> R {
        self.ml.lock();
        let r = f(self);
        self.ml.unlock();
        r
    }

    fn is_good(&mut self) -> bool {
        self.locked(|c| c.ctx.get_state().is_good())
    }

    /// Polls `op` until it finishes (`true` if it completed). The operation is released
    /// under the lock.
    fn wait<T: ?Sized>(&mut self, op: Operation<T>) -> bool {
        let deadline = Instant::now() + OP_TIMEOUT;
        let mut op = Some(op);
        loop {
            let (state, good) = self.locked(|c| {
                let st = op.as_ref().map_or(OpState::Done, |o| o.get_state());
                if st != OpState::Running {
                    op = None; // drop under lock
                }
                (st, c.ctx.get_state().is_good())
            });
            match state {
                OpState::Done => return true,
                OpState::Cancelled => return false,
                OpState::Running if !good || Instant::now() > deadline => {
                    self.locked(|_| {
                        if let Some(mut o) = op.take() {
                            o.cancel();
                        }
                    });
                    return false;
                }
                OpState::Running => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    /// Lists sink inputs and sinks.
    fn list(&mut self) -> anyhow::Result<(Vec<SinkInputRec>, HashMap<u32, u32>)> {
        let inputs: Arc<Mutex<Vec<SinkInputRec>>> = Arc::default();
        let monitors: Arc<Mutex<HashMap<u32, u32>>> = Arc::default();
        let failed = Arc::new(AtomicBool::new(false));

        let (op_inputs, op_sinks) = self.locked(|c| {
            let intro = c.ctx.introspect();
            let (inputs, failed1) = (inputs.clone(), failed.clone());
            let op_inputs = intro.get_sink_input_info_list(move |r| match r {
                ListResult::Item(i) => {
                    let pid = i
                        .proplist
                        .get_str(properties::APPLICATION_PROCESS_ID)
                        .and_then(|s| s.trim().parse().ok());
                    inputs.lock().unwrap().push(SinkInputRec {
                        index: i.index,
                        sink: i.sink,
                        pid,
                        app_name: i.proplist.get_str(properties::APPLICATION_NAME),
                        binary: i.proplist.get_str(properties::APPLICATION_PROCESS_BINARY),
                        volume: i.volume,
                        volume_writable: i.has_volume && i.volume_writable,
                    });
                }
                ListResult::End => {}
                ListResult::Error => failed1.store(true, Ordering::Relaxed),
            });
            let (monitors, failed2) = (monitors.clone(), failed.clone());
            let op_sinks = intro.get_sink_info_list(move |r| match r {
                ListResult::Item(s) => {
                    monitors.lock().unwrap().insert(s.index, s.monitor_source);
                }
                ListResult::End => {}
                ListResult::Error => failed2.store(true, Ordering::Relaxed),
            });
            (op_inputs, op_sinks)
        });
        let ok = self.wait(op_inputs) & self.wait(op_sinks);
        if !ok || failed.load(Ordering::Relaxed) {
            bail!("listing PulseAudio streams failed");
        }
        let inputs = std::mem::take(&mut *inputs.lock().unwrap());
        let monitors = std::mem::take(&mut *monitors.lock().unwrap());
        Ok((inputs, monitors))
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.ml.lock();
        self.ctx.disconnect();
        self.ml.unlock();
        // Must be called without the lock; afterwards nothing runs concurrently with the
        // remaining (lock-free) teardown of `ctx` and `ml`.
        self.ml.stop();
    }
}

/// What we keep of a sink input.
#[derive(Clone, Debug)]
struct SinkInputRec {
    index: u32,
    sink: u32,
    pid: Option<u32>,
    app_name: Option<String>,
    binary: Option<String>,
    volume: ChannelVolumes,
    volume_writable: bool,
}

// ---------------------------------------------------------------------------------------------
// Public entry points

pub(crate) fn capabilities() -> Capabilities {
    // Per-stream monitors are supported by every PulseAudio and pipewire-pulse version we
    // could meet; what can be missing is the server itself.
    let ok = match Conn::connect("Chatter capability probe") {
        Ok(_) => true,
        Err(e) => {
            log::info!("appaudio: no PulseAudio server: {e:#}");
            false
        }
    };
    Capabilities {
        per_app: ok,
        all_except: ok,
    }
}

pub(crate) fn list_apps(exclude: &[u32]) -> Vec<AudioApp> {
    let result = Conn::connect("Chatter app list").and_then(|mut c| c.list());
    let (inputs, _) = match result {
        Ok(r) => r,
        Err(e) => {
            log::warn!("appaudio: listing PulseAudio streams failed: {e:#}");
            return Vec::new();
        }
    };
    let table = snapshot();
    let excluded = table.trees(exclude);
    let mut apps = Vec::new();
    for si in inputs {
        let Some(pid) = si.pid else { continue };
        if pid == 0 || excluded.contains(&pid) {
            continue;
        }
        let root = table.app_root(pid);
        let name = si
            .app_name
            .filter(|n| !n.trim().is_empty())
            .or_else(|| si.binary.map(|b| title_from_exe(&b)))
            .or_else(|| table.get(root).map(|e| title_from_exe(&e.exe)))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("Process {root}"));
        apps.push(AudioApp { pid: root, name });
    }
    apps
}

// ---------------------------------------------------------------------------------------------
// Capture

/// Which sink inputs a target wants, given a fresh process table.
enum Matcher {
    App { pid: u32, table: ProcTable },
    AllExcept { excluded: HashSet<u32> },
}

impl Matcher {
    fn new(target: &Target) -> Matcher {
        let table = snapshot();
        match target {
            Target::App { pid } => Matcher::App { pid: *pid, table },
            Target::AllExcept { pids } => {
                let roots: Vec<u32> = if pids.is_empty() {
                    vec![std::process::id()]
                } else {
                    pids.clone()
                };
                Matcher::AllExcept {
                    excluded: table.trees(&roots),
                }
            }
        }
    }

    fn wants(&self, si: &SinkInputRec) -> bool {
        match self {
            Matcher::App { pid, table } => si.pid.is_some_and(|p| table.in_tree(p, *pid)),
            // Streams without a pid (e.g. module-generated) are not Chatter's: include them.
            Matcher::AllExcept { excluded } => !si.pid.is_some_and(|p| excluded.contains(&p)),
        }
    }
}

/// One per-sink-input monitor record stream plus its jitter buffer.
struct Monitor {
    stream: Stream,
    sink: u32,
    buf: VecDeque<f32>,
    primed: bool,
}

impl Monitor {
    /// Opens a monitor stream for `si` on `monitor_source`. Lock must be held.
    fn open(ctx: &mut Context, si: &SinkInputRec, monitor_source: u32) -> anyhow::Result<Monitor> {
        let spec = Spec {
            format: Format::F32le,
            rate: SAMPLE_RATE,
            channels: 2,
        };
        let mut stream = Stream::new(ctx, "Chatter screen-share audio", &spec, None)
            .ok_or_else(|| anyhow!("pa_stream_new failed"))?;
        stream
            .set_monitor_stream(si.index)
            .map_err(|e| anyhow!("set_monitor_stream: {e}"))?;
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: FRAGSIZE,
        };
        // DONT_MOVE: if the sink input moves to another sink our monitor must fail rather than
        // silently follow the wrong monitor source; the subscription refresh reopens it.
        let flags = StreamFlags::DONT_MOVE | StreamFlags::ADJUST_LATENCY;
        stream
            .connect_record(Some(&monitor_source.to_string()), Some(&attr), flags)
            .map_err(|e| anyhow!("connect_record: {e}"))?;
        Ok(Monitor {
            stream,
            sink: si.sink,
            buf: VecDeque::with_capacity(MAX_BUFFER_SAMPLES + CHUNK_SAMPLES),
            primed: false,
        })
    }

    /// Moves everything readable into `buf`. Lock must be held. `false` if the stream died.
    fn drain(&mut self) -> bool {
        match self.stream.get_state() {
            StreamState::Ready => {}
            StreamState::Creating | StreamState::Unconnected => return true,
            StreamState::Failed | StreamState::Terminated => return false,
        }
        loop {
            match self.stream.peek() {
                Ok(PeekResult::Empty) => break,
                Ok(PeekResult::Hole(n)) => {
                    self.buf.extend(std::iter::repeat_n(0.0, n / 4));
                    let _ = self.stream.discard();
                }
                Ok(PeekResult::Data(bytes)) => {
                    self.buf.extend(
                        bytes
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    );
                    let _ = self.stream.discard();
                }
                Err(_) => return false,
            }
        }
        // Keep whole frames and bounded latency.
        if self.buf.len() > MAX_BUFFER_SAMPLES {
            let excess = self.buf.len() - PRIME_SAMPLES;
            self.buf.drain(..excess - excess % 2);
        }
        true
    }

    /// Adds up to one chunk into `mix`.
    fn mix_into(&mut self, mix: &mut [f32]) {
        if !self.primed {
            if self.buf.len() < PRIME_SAMPLES {
                return;
            }
            self.primed = true;
        }
        let n = mix.len().min(self.buf.len());
        for (m, s) in mix.iter_mut().zip(self.buf.drain(..n)) {
            *m += s;
        }
        if n < mix.len() {
            // Underrun (stream paused or starved): re-buffer before playing again.
            self.primed = false;
        }
    }
}

/// A live connection plus the monitors opened on it.
struct Session {
    monitors: HashMap<u32, Monitor>,
    /// Sink inputs whose monitor failed; not retried until they disappear from the list.
    failed: HashSet<u32>,
    dirty: Arc<AtomicBool>,
    last_refresh: Option<Instant>,
    conn: Conn,
}

impl Session {
    fn open() -> anyhow::Result<Session> {
        let mut conn = Conn::connect("Chatter screen-share audio")?;
        let dirty = Arc::new(AtomicBool::new(true));
        let flag = dirty.clone();
        conn.locked(|c| {
            c.ctx
                .set_subscribe_callback(Some(Box::new(move |_facility, _op, _index| {
                    flag.store(true, Ordering::Relaxed);
                })));
            // The returned operation only reports whether subscribing worked; release it.
            drop(
                c.ctx
                    .subscribe(InterestMaskSet::SINK_INPUT | InterestMaskSet::SINK, |_| {}),
            );
        });
        Ok(Session {
            monitors: HashMap::new(),
            failed: HashSet::new(),
            dirty,
            last_refresh: None,
            conn,
        })
    }

    /// Re-lists streams if something changed, opening / closing monitors to match `target`.
    fn refresh(&mut self, target: &Target) -> anyhow::Result<()> {
        if !self.dirty.load(Ordering::Relaxed)
            || self
                .last_refresh
                .is_some_and(|t| t.elapsed() < REFRESH_MIN_INTERVAL)
        {
            return Ok(());
        }
        self.dirty.store(false, Ordering::Relaxed);
        self.last_refresh = Some(Instant::now());

        let (inputs, sink_monitors) = self.conn.list()?;
        let matcher = Matcher::new(target);
        let wanted: HashMap<u32, &SinkInputRec> = inputs
            .iter()
            .filter(|si| matcher.wants(si))
            .map(|si| (si.index, si))
            .collect();
        let listed: HashSet<u32> = inputs.iter().map(|si| si.index).collect();
        self.failed.retain(|i| listed.contains(i));

        let Session {
            monitors,
            failed,
            conn,
            ..
        } = self;
        conn.locked(|c| {
            // Close monitors no longer wanted or whose sink input moved.
            monitors.retain(|idx, m| {
                let keep = wanted.get(idx).is_some_and(|si| si.sink == m.sink);
                if !keep {
                    let _ = m.stream.disconnect();
                }
                keep
            });
            for (idx, si) in &wanted {
                if monitors.contains_key(idx) || failed.contains(idx) {
                    continue;
                }
                let Some(&source) = sink_monitors.get(&si.sink) else {
                    continue;
                };
                match Monitor::open(&mut c.ctx, si, source) {
                    Ok(m) => {
                        log::debug!("appaudio: monitoring sink input {idx} (pid {:?})", si.pid);
                        monitors.insert(*idx, m);
                    }
                    Err(e) => {
                        log::warn!("appaudio: cannot monitor sink input {idx}: {e:#}");
                        failed.insert(*idx);
                    }
                }
            }
        });
        Ok(())
    }

    /// Drains all monitors; dead ones are dropped and remembered as failed.
    fn drain(&mut self) {
        let Session {
            monitors,
            failed,
            conn,
            ..
        } = self;
        conn.locked(|_| {
            monitors.retain(|idx, m| {
                let alive = m.drain();
                if !alive {
                    failed.insert(*idx);
                }
                alive
            });
        });
    }

    fn mix(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        for m in self.monitors.values_mut() {
            m.mix_into(out);
        }
        for s in out.iter_mut() {
            *s = clamp(*s);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let monitors = &mut self.monitors;
        self.conn.locked(|_| {
            for m in monitors.values_mut() {
                let _ = m.stream.disconnect();
            }
            monitors.clear();
        });
    }
}

pub(crate) struct PulseCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PulseCapture {
    pub(crate) fn start(target: Target, on_audio: OnAudio) -> anyhow::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<()>>();
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("chatter-appaudio".into())
                .spawn(move || capture_thread(target, on_audio, stop, ready_tx))
                .context("spawning capture thread")?
        };
        let ready = ready_rx
            .recv_timeout(CONNECT_TIMEOUT + OP_TIMEOUT * 3)
            .unwrap_or_else(|_| Err(anyhow!("capture thread did not start")));
        let mut cap = PulseCapture {
            stop,
            thread: Some(thread),
        };
        match ready {
            Ok(()) => Ok(cap),
            Err(e) => {
                cap.stop();
                Err(e)
            }
        }
    }
}

impl Capture for PulseCapture {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn capture_thread(
    target: Target,
    mut on_audio: OnAudio,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<anyhow::Result<()>>,
) {
    let mut session = match Session::open().and_then(|mut s| s.refresh(&target).map(|_| s)) {
        Ok(s) => Some(s),
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    log::info!("appaudio: capturing {target:?} via PulseAudio");
    let _ = ready.send(Ok(()));

    let mut ticker = Ticker::new(Instant::now());
    let mut last_reconnect = Instant::now();
    let mut chunk = vec![0f32; CHUNK_SAMPLES];

    while !stop.load(Ordering::Relaxed) {
        // Keep the server connection healthy; on loss, emit silence and retry periodically.
        if session.as_mut().is_some_and(|s| !s.conn.is_good()) {
            log::warn!("appaudio: lost PulseAudio connection; retrying");
            session = None;
            last_reconnect = Instant::now();
        }
        if session.is_none() && last_reconnect.elapsed() >= RECONNECT_EVERY {
            last_reconnect = Instant::now();
            session = Session::open().ok();
        }
        if let Some(s) = session.as_mut() {
            if let Err(e) = s.refresh(&target) {
                log::debug!("appaudio: refresh failed: {e:#}");
                s.dirty.store(true, Ordering::Relaxed);
            }
            s.drain();
        }

        for _ in 0..ticker.due(Instant::now()) {
            match session.as_mut() {
                Some(s) => s.mix(&mut chunk),
                None => chunk.fill(0.0),
            }
            on_audio(&chunk);
        }

        let sleep = ticker
            .next_deadline()
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(10));
        std::thread::sleep(sleep);
    }
}

// ---------------------------------------------------------------------------------------------
// Ducking

/// Sink-input volumes. Keys are sink-input indices (never reused within a server's lifetime);
/// volumes are full per-channel `ChannelVolumes`, scaled in the linear-amplitude domain so
/// `0.2` means the same attenuation as on Windows. Streams without a pid (module-generated)
/// are ducked too. With `flat-volumes` enabled (an old PulseAudio default, off on most current
/// distros and absent in PipeWire) a stream volume change can move the sink volume as well.
struct PulseVolumes {
    filter: Filter,
    conn: Option<Conn>,
    dirty: Arc<AtomicBool>,
    pending: Vec<Operation<dyn FnMut(bool)>>,
}

impl PulseVolumes {
    fn ensure_conn(&mut self) -> anyhow::Result<&mut Conn> {
        if self.conn.as_mut().is_some_and(|c| !c.is_good()) {
            self.drop_conn();
        }
        if self.conn.is_none() {
            let mut conn = Conn::connect("Chatter ducking")?;
            let flag = self.dirty.clone();
            conn.locked(|c| {
                c.ctx
                    .set_subscribe_callback(Some(Box::new(move |_facility, _op, _index| {
                        flag.store(true, Ordering::Relaxed);
                    })));
                drop(c.ctx.subscribe(InterestMaskSet::SINK_INPUT, |_| {}));
            });
            self.conn = Some(conn);
        }
        Ok(self.conn.as_mut().expect("just connected"))
    }

    /// Releases pending operations (under the lock) and the connection.
    fn drop_conn(&mut self) {
        if let Some(mut conn) = self.conn.take() {
            let pending = &mut self.pending;
            conn.locked(|_| pending.clear());
        }
        self.pending.clear();
    }
}

impl Drop for PulseVolumes {
    fn drop(&mut self) {
        self.drop_conn();
    }
}

impl VolumeBackend for PulseVolumes {
    type Key = u32;
    type Vol = ChannelVolumes;

    fn vol_to_vec(v: &ChannelVolumes) -> Vec<f64> {
        v.get().iter().map(|c| f64::from(c.0)).collect()
    }

    fn vol_from_vec(v: &[f64]) -> Option<ChannelVolumes> {
        let channels = u8::try_from(v.len())
            .ok()
            .filter(|n| (1..=ChannelVolumes::CHANNELS_MAX).contains(n))?;
        let mut out = ChannelVolumes::default();
        out.set_len(channels);
        for (slot, x) in out.get_mut().iter_mut().zip(v) {
            *slot = Volume(*x as u32);
        }
        out.is_valid().then_some(out)
    }

    fn scan(&mut self) -> anyhow::Result<Vec<crate::duck::Session<u32, ChannelVolumes>>> {
        let (inputs, _) = self.ensure_conn()?.list()?;
        let table = snapshot();
        Ok(inputs
            .into_iter()
            .filter(|si| si.volume_writable && self.filter.allows(si.pid, &table))
            .map(|si| crate::duck::Session {
                key: si.index,
                // What stream-restore (PulseAudio) and restore-stream (WirePlumber) remember
                // the volume by.
                app: si.app_name.or(si.binary).unwrap_or_default(),
                vol: si.volume,
            })
            .collect())
    }

    fn scaled(original: &ChannelVolumes, factor: f32) -> ChannelVolumes {
        if factor >= 1.0 {
            return *original;
        }
        let mut v = *original;
        for ch in v.get_mut() {
            let lin = VolumeLinear::from(*ch).0 * f64::from(factor.max(0.0));
            *ch = Volume::from(VolumeLinear(lin));
        }
        v
    }

    fn same(a: &ChannelVolumes, b: &ChannelVolumes) -> bool {
        a.len() == b.len()
            && a.get()
                .iter()
                .zip(b.get())
                .all(|(x, y)| x.0.abs_diff(y.0) <= 1)
    }

    fn set(&mut self, key: &u32, vol: &ChannelVolumes) -> bool {
        let Some(conn) = self.conn.as_mut() else {
            return false;
        };
        let pending = &mut self.pending;
        conn.locked(|c| {
            let op = c.ctx.introspect().set_sink_input_volume(*key, vol, None);
            pending.retain(|o| o.get_state() == OpState::Running);
            pending.push(op);
        });
        true
    }

    fn flush(&mut self) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        let deadline = Instant::now() + OP_TIMEOUT;
        let pending = &mut self.pending;
        loop {
            let done = conn.locked(|c| {
                pending.retain(|o| o.get_state() == OpState::Running);
                pending.is_empty() || !c.ctx.get_state().is_good()
            });
            if done || Instant::now() > deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn take_dirty(&mut self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }

    fn poll_interval(&self) -> Duration {
        Duration::from_millis(100)
    }
}

/// Body of the ducking thread. Connects lazily (on the first scan), so a missing server only
/// makes `set` a no-op; it is retried on later scans.
pub(crate) fn run_ducker(
    filter: Filter,
    journal: Option<std::path::PathBuf>,
    rx: mpsc::Receiver<crate::duck::Cmd>,
) {
    crate::duck::run(
        PulseVolumes {
            filter,
            conn: None,
            dirty: Arc::new(AtomicBool::new(false)),
            pending: Vec::new(),
        },
        journal,
        rx,
    );
}
