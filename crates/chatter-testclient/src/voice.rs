//! Join a Chatter voice channel with Google libwebrtc, the way the browser
//! client does (`client/src/hooks/useWebRTCVoice.ts`): one publisher peer
//! connection carrying our audio, one subscriber with 12 receive-only slots.

use crate::api::Session;
use crate::sdp::munge_voice_audio_sdp;
use crate::signaling::{self, candidate_json, Outbox};
use anyhow::{bail, Context, Result};
use chatter_media::libwebrtc::{
    audio_source::native::NativeAudioSource, audio_stream::native::NativeAudioStream,
    peer_connection_factory::native::PeerConnectionFactoryExt, prelude::*, stats::RtcStats,
    MediaType,
};
use chatter_media::{rtc_configuration, SAMPLES_PER_FRAME, SAMPLE_RATE};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

pub const VOICE_SLOT_COUNT: usize = 12;

pub enum AudioInput {
    /// A tone that is on for a second and off for a second, so the server's
    /// speaking detection has something to rank and the gaps exercise DTX.
    Tone {
        hz: f32,
    },
    Wav(PathBuf),
}

pub struct VoiceOptions {
    pub room_id: String,
    pub channel_id: String,
    pub bitrate: u32,
    pub duration: Duration,
    pub input: AudioInput,
    pub out_dir: PathBuf,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub offer_has_audio_level_ext: bool,
    pub publish_answer_ptime: Option<String>,
    pub publish_answer_maxaveragebitrate: Option<String>,
    pub publisher_connected_after_ms: Option<u128>,
    pub subscriber_connected_after_ms: Option<u128>,
    pub frames_pushed: u64,
    pub frames_pushed_per_sec: f64,
    pub send_packets_per_sec: Vec<f64>,
    pub send_kbps: Vec<f64>,
    pub saw_self_speaking: bool,
    pub saw_self_in_others_slot_maps: bool,
    pub slot_map_last: BTreeMap<usize, Option<String>>,
    /// Loudest 1 s window per slot, in dBFS, and who held the slot then.
    pub slots_heard: BTreeMap<usize, SlotHeard>,
    pub errors: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct SlotHeard {
    pub peak_dbfs: f64,
    pub user_id: Option<String>,
    pub wav: String,
}

enum Event {
    PubCandidate(String, String, i32),
    SubCandidate(String, String, i32),
    PubState(PeerConnectionState),
    SubState(PeerConnectionState),
    Track(Option<String>, MediaStreamTrack),
    SlotLevel(usize, f64),
    FramesPushed(u64),
}

/// A remote candidate, held as plain data until its description is applied.
type PendingCandidate = (String, String, i32);

fn parse_candidate(value: &Value) -> Option<PendingCandidate> {
    let c = &value["candidate"];
    let candidate = c["candidate"].as_str()?.to_string();
    if candidate.is_empty() {
        return None; // end-of-candidates
    }
    let mid = c["sdpMid"].as_str().unwrap_or("0").to_string();
    let index = c["sdpMLineIndex"].as_i64().unwrap_or(0) as i32;
    Some((candidate, mid, index))
}

async fn add_candidate(pc: &PeerConnection, (candidate, mid, index): PendingCandidate) {
    match IceCandidate::parse(&mid, index, &candidate) {
        Ok(c) => {
            if let Err(e) = pc.add_ice_candidate(c).await {
                log::debug!("add_ice_candidate: {e:?}");
            }
        }
        Err(e) => log::warn!("bad remote candidate {candidate}: {e:?}"),
    }
}

/// The bindings' `OfferOptions::default()` sets the legacy
/// `offer_to_receive_audio` to an explicit false rather than leaving it unset,
/// and libwebrtc then strips the receive direction from every audio
/// transceiver: the 12 slots go out `inactive` (so the server sends nothing)
/// and the publisher `sendonly` where a browser offers `sendrecv`.
fn offer_options() -> OfferOptions {
    OfferOptions {
        offer_to_receive_audio: true,
        ..OfferOptions::default()
    }
}

fn apply_sender_bitrate(sender: &RtpSender, bitrate: u32) {
    let mut params = sender.parameters();
    if params.encodings.is_empty() {
        params.encodings.push(RtpEncodingParameters::default());
    }
    params.encodings[0].max_bitrate = Some(bitrate as u64);
    if let Err(e) = sender.set_parameters(params) {
        // The SDP-level maxaveragebitrate still applies, as in the browser.
        log::debug!("set_parameters: {e:?}");
    }
}

fn sdp_fmtp_value(sdp: &str, key: &str) -> Option<String> {
    sdp.lines()
        .filter(|l| l.starts_with("a=fmtp:") || l.starts_with("a=ptime:"))
        .find_map(|l| {
            if key == "ptime" {
                l.strip_prefix("a=ptime:").map(str::to_string)
            } else {
                l.split([' ', ';'])
                    .find_map(|p| p.strip_prefix(&format!("{key}=")).map(str::to_string))
            }
        })
}

pub async fn run(session: &Session, opts: VoiceOptions) -> Result<Report> {
    let mut report = Report::default();
    let started = Instant::now();

    let ice = session.ice_servers().await?;
    std::fs::create_dir_all(&opts.out_dir)?;
    let (outbox, mut inbox) = signaling::connect(&session.ws_url()?, &session.access_token).await?;
    let (events_tx, mut events) = mpsc::unbounded_channel::<Event>();
    let (stop_tx, stop_rx) = watch::channel(false);

    outbox.send(json!({
        "type": "voice_join",
        "room_id": opts.room_id,
        "channel_id": opts.channel_id,
        "muted": false,
        "deafened": false,
    }));

    let factory = PeerConnectionFactory::default();

    // ── Publisher ────────────────────────────────────────────────────────
    let publisher = factory.create_peer_connection(rtc_configuration(&ice.ice_servers))?;
    {
        let tx = events_tx.clone();
        publisher.on_ice_candidate(Some(Box::new(move |c: IceCandidate| {
            let _ = tx.send(Event::PubCandidate(
                c.candidate(),
                c.sdp_mid(),
                c.sdp_mline_index(),
            ));
        })));
        let tx = events_tx.clone();
        publisher.on_connection_state_change(Some(Box::new(move |s| {
            let _ = tx.send(Event::PubState(s));
        })));
    }
    // No internal queue: frames go straight in, paced by our own 10 ms clock
    // the way a microphone callback will drive it in the engine.
    let source = NativeAudioSource::new(AudioSourceOptions::default(), SAMPLE_RATE, 1, 0);
    let track = factory.create_audio_track("chatter-spike-mic", source.clone());
    let sender = publisher
        .add_track(MediaStreamTrack::Audio(track.clone()), &["chatter-spike"])
        .context("add_track")?;

    let offer = publisher.create_offer(offer_options()).await?;
    let offer_sdp = offer.to_string();
    report.offer_has_audio_level_ext =
        offer_sdp.contains("urn:ietf:params:rtp-hdrext:ssrc-audio-level");
    let _ = std::fs::write(opts.out_dir.join("publish-offer.sdp"), &offer_sdp);
    publisher.set_local_description(offer).await?;
    apply_sender_bitrate(&sender, opts.bitrate);
    outbox.send(json!({
        "type": "voice_webrtc_publish_offer",
        "room_id": opts.room_id,
        "channel_id": opts.channel_id,
        "sdp": offer_sdp,
    }));

    // ── Subscriber: 12 receive-only slots, slot index == m-line index ────
    let subscriber = factory.create_peer_connection(rtc_configuration(&ice.ice_servers))?;
    {
        let tx = events_tx.clone();
        subscriber.on_ice_candidate(Some(Box::new(move |c: IceCandidate| {
            let _ = tx.send(Event::SubCandidate(
                c.candidate(),
                c.sdp_mid(),
                c.sdp_mline_index(),
            ));
        })));
        let tx = events_tx.clone();
        subscriber.on_connection_state_change(Some(Box::new(move |s| {
            let _ = tx.send(Event::SubState(s));
        })));
        let tx = events_tx.clone();
        subscriber.on_track(Some(Box::new(move |ev| {
            let _ = tx.send(Event::Track(ev.transceiver.mid(), ev.track));
        })));
    }
    for _ in 0..VOICE_SLOT_COUNT {
        subscriber.add_transceiver_for_media(
            MediaType::Audio,
            RtpTransceiverInit {
                direction: RtpTransceiverDirection::RecvOnly,
                stream_ids: vec![],
                send_encodings: vec![],
            },
        )?;
    }
    let sub_offer = subscriber.create_offer(offer_options()).await?;
    let sub_offer_sdp = sub_offer.to_string();
    let _ = std::fs::write(opts.out_dir.join("subscribe-offer.sdp"), &sub_offer_sdp);
    subscriber.set_local_description(sub_offer).await?;
    outbox.send(json!({
        "type": "voice_webrtc_subscribe_offer",
        "room_id": opts.room_id,
        "sdp": sub_offer_sdp,
    }));
    let slot_mids: Vec<Option<String>> =
        subscriber.transceivers().iter().map(|t| t.mid()).collect();

    // ── Our audio ────────────────────────────────────────────────────────
    let samples = load_input(&opts.input)?;
    tokio::spawn(pump_audio(
        source,
        samples,
        events_tx.clone(),
        stop_rx.clone(),
    ));

    // ── Main loop ────────────────────────────────────────────────────────
    let mut pub_answered = false;
    let mut sub_answered = false;
    let mut pub_pending: Vec<PendingCandidate> = Vec::new();
    let mut sub_pending: Vec<PendingCandidate> = Vec::new();
    let mut slot_users: BTreeMap<usize, Option<String>> = BTreeMap::new();
    let mut slot_peaks: BTreeMap<usize, (f64, Option<String>)> = BTreeMap::new();
    let mut last_out: Option<(Instant, u64, u64)> = None;
    let deadline = tokio::time::sleep(opts.duration);
    tokio::pin!(deadline);
    let mut stats_tick = tokio::time::interval(Duration::from_secs(2));

    loop {
        tokio::select! {
            _ = &mut deadline => break,
            _ = stats_tick.tick() => {
                if let Ok(stats) = publisher.get_stats().await {
                    for s in stats {
                        if let RtcStats::OutboundRtp(o) = s {
                            if o.stream.kind != "audio" { continue; }
                            let now = Instant::now();
                            if let Some((t, p, b)) = last_out {
                                let dt = now.duration_since(t).as_secs_f64();
                                let pps = (o.sent.packets_sent - p) as f64 / dt;
                                let kbps = (o.sent.bytes_sent - b) as f64 * 8.0 / dt / 1000.0;
                                log::info!("send: {pps:.1} packets/s, {kbps:.1} kbps");
                                report.send_packets_per_sec.push(pps);
                                report.send_kbps.push(kbps);
                            }
                            last_out = Some((now, o.sent.packets_sent, o.sent.bytes_sent));
                        }
                    }
                }
            }
            msg = inbox.recv() => {
                let Some(msg) = msg else { bail!("socket closed") };
                let kind = msg["type"].as_str().unwrap_or_default().to_string();
                match kind.as_str() {
                    "voice_webrtc_publish_answer" => {
                        let raw = msg["sdp"].as_str().context("answer without sdp")?;
                        let munged = munge_voice_audio_sdp(raw, opts.bitrate);
                        let _ = std::fs::write(opts.out_dir.join("publish-answer.sdp"), &munged);
                        report.publish_answer_ptime = sdp_fmtp_value(&munged, "ptime");
                        report.publish_answer_maxaveragebitrate = sdp_fmtp_value(&munged, "maxaveragebitrate");
                        let desc = SessionDescription::parse(&munged, SdpType::Answer)
                            .map_err(|e| anyhow::anyhow!("publish answer: {e:?}"))?;
                        publisher.set_remote_description(desc).await?;
                        apply_sender_bitrate(&sender, opts.bitrate);
                        pub_answered = true;
                        for c in pub_pending.drain(..) { add_candidate(&publisher, c).await; }
                        log::info!("publish answer applied");
                    }
                    "voice_webrtc_subscribe_answer" => {
                        let raw = msg["sdp"].as_str().context("answer without sdp")?;
                        let _ = std::fs::write(opts.out_dir.join("subscribe-answer.sdp"), raw);
                        let desc = SessionDescription::parse(raw, SdpType::Answer)
                            .map_err(|e| anyhow::anyhow!("subscribe answer: {e:?}"))?;
                        subscriber.set_remote_description(desc).await?;
                        sub_answered = true;
                        for c in sub_pending.drain(..) { add_candidate(&subscriber, c).await; }
                        log::info!("subscribe answer applied ({} slots)", msg["slot_count"]);
                    }
                    "voice_webrtc_publish_candidate" => if let Some(c) = parse_candidate(&msg) {
                        log::debug!("remote publish candidate: {}", c.0);
                        if pub_answered { add_candidate(&publisher, c).await } else { pub_pending.push(c) }
                    },
                    "voice_webrtc_subscribe_candidate" => if let Some(c) = parse_candidate(&msg) {
                        log::debug!("remote subscribe candidate: {}", c.0);
                        if sub_answered { add_candidate(&subscriber, c).await } else { sub_pending.push(c) }
                    },
                    "voice_slot_map" => {
                        slot_users.clear();
                        for s in msg["slots"].as_array().into_iter().flatten() {
                            let slot = s["slot"].as_u64().unwrap_or(0) as usize;
                            let user = s["user_id"].as_str().map(str::to_string);
                            if user.as_deref() == Some(session.user_id.as_str()) {
                                report.saw_self_in_others_slot_maps = true;
                            }
                            slot_users.insert(slot, user);
                        }
                        let held: Vec<String> = slot_users.iter()
                            .filter_map(|(s, u)| u.as_ref().map(|u| format!("{s}={u}")))
                            .collect();
                        log::info!("slot map: [{}]", held.join(", "));
                    }
                    "voice_speaking" => {
                        let speaking: Vec<&str> = msg["speaking"].as_array().into_iter().flatten()
                            .filter_map(Value::as_str).collect();
                        if speaking.contains(&session.user_id.as_str()) { report.saw_self_speaking = true; }
                        log::info!("speaking: {speaking:?}");
                    }
                    "voice_webrtc_error" | "error" => {
                        log::warn!("server error: {msg}");
                        report.errors.push(msg.to_string());
                    }
                    "voice_user_joined" | "voice_user_left" => log::info!("{kind}: {}", msg["user_id"]),
                    _ => log::trace!("ws <- {kind}"),
                }
            }
            Some(event) = events.recv() => match event {
                // The server drops candidates that arrive before its offer
                // handler has run; ours only exist after the offer was sent.
                Event::PubCandidate(c, mid, idx) => send_candidate(&outbox, "voice_webrtc_publish_candidate", &opts.room_id, &c, &mid, idx),
                Event::SubCandidate(c, mid, idx) => send_candidate(&outbox, "voice_webrtc_subscribe_candidate", &opts.room_id, &c, &mid, idx),
                Event::PubState(s) => {
                    log::info!("publisher: {s:?}");
                    if s == PeerConnectionState::Connected { report.publisher_connected_after_ms.get_or_insert(started.elapsed().as_millis()); }
                }
                Event::SubState(s) => {
                    log::info!("subscriber: {s:?}");
                    if s == PeerConnectionState::Connected { report.subscriber_connected_after_ms.get_or_insert(started.elapsed().as_millis()); }
                }
                Event::Track(mid, track) => {
                    let slot = mid.as_ref()
                        .and_then(|m| slot_mids.iter().position(|x| x.as_ref() == Some(m)))
                        .unwrap_or(usize::MAX);
                    log::info!("track on mid {mid:?} -> slot {slot}");
                    if let (MediaStreamTrack::Audio(audio), true) = (track, slot < VOICE_SLOT_COUNT) {
                        let path = opts.out_dir.join(format!("slot-{slot:02}.wav"));
                        tokio::spawn(record_slot(slot, audio, path, events_tx.clone(), stop_rx.clone()));
                    }
                }
                Event::SlotLevel(slot, dbfs) => {
                    let user = slot_users.get(&slot).cloned().flatten();
                    log::info!("slot {slot} ({}): {dbfs:.1} dBFS", user.as_deref().unwrap_or("empty"));
                    let entry = slot_peaks.entry(slot).or_insert((f64::NEG_INFINITY, None));
                    if dbfs > entry.0 { *entry = (dbfs, user); }
                }
                Event::FramesPushed(n) => {
                    report.frames_pushed = n;
                    report.frames_pushed_per_sec = n as f64 / started.elapsed().as_secs_f64();
                }
            },
        }
    }

    outbox.send(
        json!({ "type": "voice_leave", "room_id": opts.room_id, "channel_id": opts.channel_id }),
    );
    let _ = stop_tx.send(true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    publisher.close();
    subscriber.close();

    report.slot_map_last = slot_users;
    report.slots_heard = slot_peaks
        .into_iter()
        .map(|(slot, (peak_dbfs, user_id))| {
            let wav = opts
                .out_dir
                .join(format!("slot-{slot:02}.wav"))
                .display()
                .to_string();
            (
                slot,
                SlotHeard {
                    peak_dbfs,
                    user_id,
                    wav,
                },
            )
        })
        .collect();
    Ok(report)
}

fn send_candidate(
    outbox: &Outbox,
    kind: &str,
    room_id: &str,
    candidate: &str,
    mid: &str,
    index: i32,
) {
    outbox.send(json!({ "type": kind, "room_id": room_id, "candidate": candidate_json(candidate, mid, index) }));
}

/// Mono 48 kHz samples to loop as our microphone.
fn load_input(input: &AudioInput) -> Result<Vec<i16>> {
    match input {
        AudioInput::Tone { hz } => {
            let rate = SAMPLE_RATE as f32;
            Ok((0..SAMPLE_RATE as usize * 2)
                .map(|i| {
                    if i >= SAMPLE_RATE as usize {
                        return 0; // second half silent
                    }
                    let t = i as f32 / rate;
                    ((t * hz * std::f32::consts::TAU).sin() * 0.25 * i16::MAX as f32) as i16
                })
                .collect())
        }
        AudioInput::Wav(path) => read_wav_mono_48k(path),
    }
}

fn read_wav_mono_48k(path: &Path) -> Result<Vec<i16>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()?
        }
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
    };
    let mono: Vec<f32> = raw
        .chunks(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect();
    // Linear resampling is plenty for a test signal.
    let ratio = spec.sample_rate as f64 / SAMPLE_RATE as f64;
    let out_len = (mono.len() as f64 / ratio) as usize;
    Ok((0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let j = pos as usize;
            let frac = (pos - j as f64) as f32;
            let a = mono[j.min(mono.len() - 1)];
            let b = mono[(j + 1).min(mono.len() - 1)];
            ((a + (b - a) * frac).clamp(-1.0, 1.0) * i16::MAX as f32) as i16
        })
        .collect())
}

/// Push 10 ms frames into libwebrtc in real time, looping the input.
async fn pump_audio(
    source: NativeAudioSource,
    samples: Vec<i16>,
    events: mpsc::UnboundedSender<Event>,
    stop: watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    let mut pos = 0usize;
    let mut pushed = 0u64;
    let mut frame = AudioFrame::new(SAMPLE_RATE, 1, SAMPLES_PER_FRAME as u32);
    while !*stop.borrow() {
        tick.tick().await;
        let data = frame.data.to_mut();
        for s in data.iter_mut() {
            *s = samples[pos];
            pos = (pos + 1) % samples.len();
        }
        if let Err(e) = source.capture_frame(&frame).await {
            log::warn!("capture_frame: {e:?}");
        }
        pushed += 1;
        if pushed.is_multiple_of(100) {
            let _ = events.send(Event::FramesPushed(pushed));
        }
    }
}

/// Write one slot's audio to a WAV and report its level once a second.
async fn record_slot(
    slot: usize,
    track: RtcAudioTrack,
    path: PathBuf,
    events: mpsc::UnboundedSender<Event>,
    mut stop: watch::Receiver<bool>,
) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = match hound::WavWriter::create(&path, spec) {
        Ok(w) => w,
        Err(e) => return log::warn!("slot {slot}: {e}"),
    };
    let mut stream = NativeAudioStream::new(track, SAMPLE_RATE as i32, 1);
    let (mut sum_sq, mut count) = (0f64, 0usize);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            frame = stream.next() => {
                let Some(frame) = frame else { break };
                for &s in frame.data.iter() {
                    let _ = writer.write_sample(s);
                    let v = s as f64 / i16::MAX as f64;
                    sum_sq += v * v;
                }
                count += frame.data.len();
                if count >= SAMPLE_RATE as usize {
                    let rms = (sum_sq / count as f64).sqrt();
                    let dbfs = if rms > 0.0 { 20.0 * rms.log10() } else { -120.0 };
                    if dbfs > -60.0 { let _ = events.send(Event::SlotLevel(slot, dbfs)); }
                    sum_sq = 0.0;
                    count = 0;
                }
            }
        }
    }
    stream.close();
    if let Err(e) = writer.finalize() {
        log::warn!("slot {slot}: {e}");
    }
}
