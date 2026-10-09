//! Request dispatch and the state behind it: peers, open mics, the playout
//! mixer, the mic test and push-to-talk.

use crate::capture::{self, Capture, MicOptions, SharedApm};
use crate::devices;
use crate::peers::{IceServerArg, Peers};
use crate::playout::{Playout, Position};
use crate::protocol::{Out, Request};
use chatter_appaudio::{AppAudioCapture, Ducker, Target};
use chatter_hotkeys::{Backend, Binding, HotkeyWatcher};
use chatter_media::libwebrtc::{
    audio_source::native::NativeAudioSource, audio_stream::native::NativeAudioStream, prelude::*,
};
use chatter_media::{SAMPLES_PER_FRAME, SAMPLE_RATE};
use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct Mic {
    _capture: Capture,
    capture_options: MicOptions,
    source: NativeAudioSource,
    enabled: Arc<AtomicBool>,
}

struct MicTest {
    capture: Capture,
    options: MicOptions,
    monitoring: Arc<AtomicBool>,
}

pub struct Engine {
    out: Out,
    peers: Peers,
    apm: SharedApm,
    playout: Option<Playout>,
    output: (String, f32),
    slot_feeds: HashMap<u32, tokio::task::JoinHandle<()>>,
    mics: HashMap<String, Mic>,
    test: Option<MicTest>,
    hotkeys: Option<HotkeyWatcher>,
    app_audio: HashMap<u32, AppAudioCapture>,
    next_stream: u32,
    ducking: Arc<parking_lot::Mutex<Ducking>>,
}

/// Turning other apps down while the call is audible.
#[derive(Default)]
struct Ducking {
    /// 0 (off) to 1 (silence them).
    amount: f32,
    talking: bool,
    ducker: Option<Ducker>,
    /// What was last asked of the ducker, to log changes only.
    applied: Option<Option<u32>>,
}

/// Where the ducker records what it has turned down, so a run that was killed
/// is undone by the next: the app's profile when it says, else the temp dir.
fn ducking_journal() -> std::path::PathBuf {
    std::env::var_os("CHATTER_ENGINE_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("chatter-ducking.json")
}

impl Ducking {
    /// A previous run left other apps turned down: start the ducker now, which
    /// puts them back, rather than waiting for ducking to be switched on.
    fn recovering() -> Self {
        let mut ducking = Ducking::default();
        if ducking_journal().exists() {
            ducking.start();
        }
        ducking
    }

    fn start(&mut self) {
        // Never our own sound: the engine, and the app that started it.
        let mut own = vec![std::process::id()];
        own.extend(crate::processes::parent_pid());
        self.ducker = Some(Ducker::new(own, Some(ducking_journal())));
    }

    fn apply(&mut self) {
        if self.amount > 0.0 && self.ducker.is_none() {
            self.start();
        }
        if let Some(ducker) = &self.ducker {
            let duck = self.talking && self.amount > 0.0;
            let level = duck.then_some(1.0 - self.amount);
            let key = level.map(|l| (l * 100.0).round() as u32);
            if self.applied != Some(key) {
                match level {
                    Some(l) => log::info!("ducking other apps to {:.0}%", l * 100.0),
                    None => log::info!("restoring other apps' volume"),
                }
                self.applied = Some(key);
            }
            ducker.set(level);
        }
    }
}

fn arg<T: DeserializeOwned>(args: &Value, name: &str) -> Result<T, String> {
    serde_json::from_value(args.get(name).cloned().unwrap_or(Value::Null))
        .map_err(|e| format!("{name}: {e}"))
}

impl Engine {
    pub fn new(out: Out) -> Self {
        let hotkeys = {
            let out = out.clone();
            match HotkeyWatcher::start(move |down| out.event("ptt", json!({ "down": down }))) {
                Ok(w) => Some(w),
                Err(e) => {
                    log::warn!("push-to-talk unavailable: {e}");
                    None
                }
            }
        };
        Self {
            peers: Peers::new(out.clone()),
            out,
            apm: SharedApm::default(),
            playout: None,
            output: ("default".into(), 1.0),
            slot_feeds: HashMap::new(),
            mics: HashMap::new(),
            test: None,
            hotkeys,
            app_audio: HashMap::new(),
            next_stream: 1,
            ducking: Arc::new(parking_lot::Mutex::new(Ducking::recovering())),
        }
    }

    pub fn hello(&self) -> Value {
        let backend = self.hotkeys.as_ref().map(|h| h.backend());
        let mut features = vec!["voice"];
        if !matches!(backend, None | Some(Backend::Unsupported)) {
            features.push("ptt");
        }
        let caps = chatter_appaudio::capabilities();
        if caps.per_app || caps.all_except {
            features.push("app-audio");
        }
        if chatter_appaudio::ducking_supported() {
            features.push("ducking");
        }
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "features": features,
            "hotkeyBackend": format!("{:?}", backend.unwrap_or(Backend::Unsupported)),
        })
    }

    fn playout(&mut self) -> &Playout {
        if self.playout.is_none() {
            let ducking = self.ducking.clone();
            let on_speech: crate::playout::SpeechListener = Box::new(move |talking| {
                let mut d = ducking.lock();
                d.talking = talking;
                d.apply();
            });
            let playout = Playout::start(self.apm.clone(), Some(on_speech));
            playout.set_output(&self.output.0, self.output.1);
            self.playout = Some(playout);
        }
        self.playout.as_ref().expect("started above")
    }

    pub async fn handle(&mut self, req: &Request) -> Result<Value, String> {
        let a = &req.args;
        match req.op.as_str() {
            "devices.list" => Ok(serde_json::to_value(devices::list()).unwrap_or_default()),

            "mic.open" => {
                let mic_id: String = arg(a, "micId")?;
                let options: MicOptions = arg(a, "options")?;
                self.open_mic(mic_id, options);
                Ok(Value::Null)
            }
            "mic.update" => {
                let mic_id: String = arg(a, "micId")?;
                let options: MicOptions = arg(a, "options")?;
                let mic = self.mics.get_mut(&mic_id).ok_or("no such mic")?;
                mic._capture.update(options.clone());
                mic.capture_options = options;
                Ok(Value::Null)
            }
            "mic.enable" => {
                let mic_id: String = arg(a, "micId")?;
                let on: bool = arg(a, "on")?;
                if let Some(mic) = self.mics.get(&mic_id) {
                    mic.enabled.store(on, Ordering::Release);
                }
                self.peers.set_mic_enabled(&mic_id, on);
                Ok(Value::Null)
            }
            "mic.close" => {
                let mic_id: String = arg(a, "micId")?;
                self.mics.remove(&mic_id);
                Ok(Value::Null)
            }

            "peer.create" => {
                let peer_id: String = arg(a, "peerId")?;
                let ice: Vec<IceServerArg> = arg(a, "iceServers").unwrap_or_default();
                self.peers.create(&peer_id, ice)
            }
            "peer.addRecvOnlyAudio" => self.peers.add_recv_only_audio(&arg::<String>(a, "peerId")?),
            "peer.attachMic" => {
                let peer_id: String = arg(a, "peerId")?;
                let mic_id: String = arg(a, "micId")?;
                let mic = self.mics.get(&mic_id).ok_or("no such mic")?;
                let enabled = mic.enabled.load(Ordering::Acquire);
                self.peers
                    .attach_mic(&peer_id, &mic_id, &mic.source.clone(), enabled)
            }
            "peer.createOffer" => self.peers.create_offer(&arg::<String>(a, "peerId")?).await,
            "peer.setLocal" | "peer.setRemote" => {
                let peer_id: String = arg(a, "peerId")?;
                let kind: String = arg(a, "type")?;
                let sdp: String = arg(a, "sdp")?;
                self.peers
                    .set_description(&peer_id, req.op == "peer.setLocal", &kind, &sdp)
                    .await
            }
            "peer.addIce" => {
                let peer_id: String = arg(a, "peerId")?;
                let candidate: String = arg(a, "candidate")?;
                let mid: String = arg(a, "sdpMid").unwrap_or_else(|_| "0".into());
                let index: i32 = arg(a, "sdpMLineIndex").unwrap_or(0);
                self.peers.add_ice(&peer_id, &candidate, &mid, index).await
            }
            "peer.restartIce" => self.peers.restart_ice(&arg::<String>(a, "peerId")?),
            "peer.setMaxBitrate" => {
                let peer_id: String = arg(a, "peerId")?;
                let bps: f64 = arg(a, "bps")?;
                self.peers.set_max_bitrate(&peer_id, bps as u64)
            }
            "peer.getStats" => self.peers.stats(&arg::<String>(a, "peerId")?).await,
            "peer.close" => self.peers.close(&arg::<String>(a, "peerId")?),

            "playout.attach" => {
                let slot: u32 = arg(a, "slot")?;
                let peer_id: String = arg(a, "peerId")?;
                let index: usize = arg(a, "index")?;
                let track = self.peers.track(&peer_id, index).ok_or("no such track")?;
                self.attach_slot(slot, track);
                Ok(Value::Null)
            }
            "playout.gain" => {
                let slot: u32 = arg(a, "slot")?;
                let gain: f32 = arg(a, "gain")?;
                if let Some(p) = &self.playout {
                    p.set_gain(slot, gain);
                }
                Ok(Value::Null)
            }
            "playout.position" => {
                let slot: u32 = arg(a, "slot")?;
                let at = Position {
                    x: arg(a, "x")?,
                    y: arg(a, "y")?,
                    z: arg(a, "z")?,
                };
                if let Some(p) = &self.playout {
                    p.set_position(slot, at);
                }
                Ok(Value::Null)
            }
            "playout.listener" => {
                let at = Position {
                    x: arg(a, "x")?,
                    y: arg(a, "y")?,
                    z: arg(a, "z")?,
                };
                if let Some(p) = &self.playout {
                    p.set_listener(at);
                }
                Ok(Value::Null)
            }
            "playout.output" => {
                let device_id: String = arg(a, "deviceId")?;
                let volume: f32 = arg(a, "volume")?;
                self.output = (device_id, volume);
                if let Some(p) = &self.playout {
                    p.set_output(&self.output.0, self.output.1);
                }
                Ok(Value::Null)
            }
            "playout.close" => {
                self.close_playout();
                Ok(Value::Null)
            }

            "test.start" => {
                let options: MicOptions = arg(a, "options")?;
                let output: String = arg(a, "outputDeviceId").unwrap_or_else(|_| "default".into());
                self.start_test(options, output);
                Ok(Value::Null)
            }
            "test.monitor" => {
                let on: bool = arg(a, "on")?;
                if let Some(test) = &self.test {
                    test.monitoring.store(on, Ordering::Release);
                }
                Ok(Value::Null)
            }
            "test.gain" => {
                let gain: f32 = arg(a, "gain")?;
                if let Some(test) = &mut self.test {
                    test.options.gain = gain;
                    test.capture.update(test.options.clone());
                }
                Ok(Value::Null)
            }
            "test.stop" => {
                self.stop_test();
                Ok(Value::Null)
            }

            "ptt.set" => {
                let binding: Option<Binding> = arg(a, "binding")?;
                let Some(hotkeys) = &self.hotkeys else {
                    return Ok(Value::Null);
                };
                hotkeys.set_binding(binding.clone());
                Ok(binding.map_or(
                    Value::Null,
                    |b| json!({ "code": b.code, "label": b.label() }),
                ))
            }

            "appaudio.caps" => {
                Ok(serde_json::to_value(chatter_appaudio::capabilities()).unwrap_or_default())
            }
            "appaudio.list" => {
                let exclude: Vec<u32> = arg(a, "excludePids").unwrap_or_default();
                Ok(serde_json::to_value(chatter_appaudio::list_apps(&exclude)).unwrap_or_default())
            }
            "appaudio.windowPid" => {
                let source: String = arg(a, "sourceId")?;
                Ok(json!(chatter_appaudio::window_pid(&source)))
            }
            "appaudio.start" => {
                let kind: String = arg(&a["target"], "kind")?;
                let target = match kind.as_str() {
                    "app" => Target::App {
                        pid: arg(&a["target"], "pid")?,
                    },
                    "allExcept" => Target::AllExcept {
                        pids: arg(&a["target"], "pids")?,
                    },
                    other => return Err(format!("unknown audio target {other}")),
                };
                let stream = self.next_stream;
                self.next_stream += 1;
                let out = self.out.clone();
                let mut bytes = Vec::with_capacity(960 * 4);
                let capture = AppAudioCapture::start(target, move |samples: &[f32]| {
                    bytes.clear();
                    for s in samples {
                        bytes.extend_from_slice(&s.to_le_bytes());
                    }
                    out.binary(stream, &bytes);
                })
                .map_err(|e| e.to_string())?;
                self.app_audio.insert(stream, capture);
                Ok(json!({ "stream": stream }))
            }
            "appaudio.stop" => {
                let stream: u32 = arg(a, "stream")?;
                self.app_audio.remove(&stream);
                Ok(Value::Null)
            }

            "ducking.set" => {
                let amount: f32 = arg(a, "amount")?;
                let mut d = self.ducking.lock();
                d.amount = amount.clamp(0.0, 1.0);
                d.apply();
                Ok(Value::Null)
            }

            "processes.list" => {
                Ok(serde_json::to_value(crate::processes::list()).unwrap_or_default())
            }

            "session.reset" => {
                self.app_audio.clear();
                self.peers.close_all();
                self.mics.clear();
                self.stop_test();
                self.close_playout();
                Ok(Value::Null)
            }

            other => Err(format!("unknown op {other}")),
        }
    }

    fn open_mic(&mut self, mic_id: String, options: MicOptions) {
        let source = NativeAudioSource::new(AudioSourceOptions::default(), SAMPLE_RATE, 1, 0);
        let enabled = Arc::new(AtomicBool::new(true));

        let sink_source = source.clone();
        let mut frame = vec![0i16; SAMPLES_PER_FRAME];
        let sink: capture::FrameSink = Box::new(move |samples: &[f32]| {
            for (dst, src) in frame.iter_mut().zip(samples) {
                *dst = (src * i16::MAX as f32) as i16;
            }
            let audio = AudioFrame {
                data: Cow::Borrowed(&frame),
                sample_rate: SAMPLE_RATE,
                num_channels: 1,
                samples_per_channel: SAMPLES_PER_FRAME as u32,
            };
            // With no internal queue this completes immediately.
            if let Err(e) = futures_executor::block_on(sink_source.capture_frame(&audio)) {
                log::debug!("capture_frame: {e:?}");
            }
        });

        let out = self.out.clone();
        let level_id = mic_id.clone();
        let level_enabled = enabled.clone();
        let levels: capture::LevelSink = Box::new(move |level, speaking| {
            let on = level_enabled.load(Ordering::Acquire);
            out.event(
                "mic.level",
                json!({ "micId": level_id, "level": level, "speaking": speaking && on }),
            );
        });

        let capture = Capture::start(options.clone(), self.apm.clone(), sink, levels);
        self.mics.insert(
            mic_id,
            Mic {
                _capture: capture,
                capture_options: options,
                source,
                enabled,
            },
        );
    }

    fn attach_slot(&mut self, slot: u32, track: RtcAudioTrack) {
        let mut feed = self.playout().attach(slot);
        if let Some(previous) = self.slot_feeds.remove(&slot) {
            previous.abort();
        }
        let task = tokio::spawn(async move {
            let mut stream = NativeAudioStream::new(track, SAMPLE_RATE as i32, 1);
            let mut samples = Vec::with_capacity(SAMPLES_PER_FRAME);
            while let Some(frame) = stream.next().await {
                samples.clear();
                samples.extend(frame.data.iter().map(|s| *s as f32 / i16::MAX as f32));
                // A full slot means the mixer has stopped draining it.
                let _ = feed.push_partial_slice(&samples);
            }
        });
        self.slot_feeds.insert(slot, task);
    }

    fn close_playout(&mut self) {
        for (_, task) in self.slot_feeds.drain() {
            task.abort();
        }
        // No call, nobody talking: put other apps back.
        {
            let mut d = self.ducking.lock();
            d.talking = false;
            d.apply();
        }
        if self.test.is_none() {
            self.playout = None;
        } else if let Some(p) = &self.playout {
            p.clear_slots();
        }
    }

    fn start_test(&mut self, options: MicOptions, output_device: String) {
        self.stop_test();
        let monitoring = Arc::new(AtomicBool::new(false));
        let (mut monitor_in, monitor_out) = rtrb::RingBuffer::<f32>::new(SAMPLE_RATE as usize / 2);
        // The test plays through the speakers it was asked about; a call that
        // is running keeps its own choice.
        if self.slot_feeds.is_empty() {
            self.output.0 = output_device;
        }
        self.playout().set_monitor(Some(monitor_out));
        if let Some(p) = &self.playout {
            p.set_output(&self.output.0, self.output.1);
        }

        let sink_monitoring = monitoring.clone();
        let sink: capture::FrameSink = Box::new(move |samples: &[f32]| {
            if sink_monitoring.load(Ordering::Acquire) {
                let _ = monitor_in.push_partial_slice(samples);
            }
        });
        let out = self.out.clone();
        let levels: capture::LevelSink = Box::new(move |rms, _| {
            // The browser's meter scale: byte RMS × 5, capped at 100.
            out.event("test.level", json!({ "level": (rms * 640.0).min(100.0) }));
        });
        // Its own processing, so the test never retunes the call's echo
        // canceller.
        let capture = Capture::start(options.clone(), SharedApm::default(), sink, levels);
        self.test = Some(MicTest {
            capture,
            options,
            monitoring,
        });
    }

    fn stop_test(&mut self) {
        if self.test.take().is_some() {
            if let Some(p) = &self.playout {
                p.set_monitor(None);
            }
            if self.slot_feeds.is_empty() {
                self.playout = None;
            }
        }
    }
}
