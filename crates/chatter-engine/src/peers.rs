//! Peer connections on Google libwebrtc, driven op by op from the client's
//! voice hook (via the desktop app's VoicePeer stand-in). Everything Chatter-
//! specific — which offers to send, munging the answer, retries — stays in the
//! client; this only does what a browser's RTCPeerConnection would.

use crate::protocol::Out;
use chatter_media::libwebrtc::{
    audio_source::native::NativeAudioSource,
    peer_connection_factory::native::PeerConnectionFactoryExt, prelude::*, stats::RtcStats,
    MediaType,
};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct IceServerArg {
    pub urls: Vec<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub credential: String,
}

struct Peer {
    pc: PeerConnection,
    /// Remote audio by transceiver index — the client's slot numbering.
    tracks: Arc<Mutex<HashMap<usize, RtcAudioTrack>>>,
    senders: Vec<RtpSender>,
    /// Our mic's track on this peer, for mute.
    mic_tracks: Vec<(String, RtcAudioTrack)>,
}

pub struct Peers {
    factory: PeerConnectionFactory,
    peers: HashMap<String, Peer>,
    out: Out,
}

/// The bindings' `OfferOptions::default()` sets the legacy
/// `offer_to_receive_audio` to an explicit false, and libwebrtc then strips
/// the receive direction from every audio transceiver (the voice slots go out
/// `inactive`). See docs/SPIKE-RESULTS.md.
fn offer_options() -> OfferOptions {
    OfferOptions {
        offer_to_receive_audio: true,
        ..OfferOptions::default()
    }
}

fn state_name(state: PeerConnectionState) -> &'static str {
    match state {
        PeerConnectionState::New => "new",
        PeerConnectionState::Connecting => "connecting",
        PeerConnectionState::Connected => "connected",
        PeerConnectionState::Disconnected => "disconnected",
        PeerConnectionState::Failed => "failed",
        PeerConnectionState::Closed => "closed",
    }
}

fn err(e: impl std::fmt::Debug) -> String {
    format!("{e:?}")
}

impl Peers {
    pub fn new(out: Out) -> Self {
        Self {
            factory: PeerConnectionFactory::default(),
            peers: HashMap::new(),
            out,
        }
    }

    fn peer(&self, peer_id: &str) -> Result<&Peer, String> {
        self.peers
            .get(peer_id)
            .ok_or_else(|| format!("no peer {peer_id}"))
    }

    pub fn create(
        &mut self,
        peer_id: &str,
        ice_servers: Vec<IceServerArg>,
    ) -> Result<Value, String> {
        let mut config = RtcConfiguration::default();
        config.ice_servers = ice_servers
            .into_iter()
            .map(|s| IceServer {
                urls: s.urls,
                username: s.username,
                password: s.credential,
            })
            .collect();
        let pc = self.factory.create_peer_connection(config).map_err(err)?;
        let tracks: Arc<Mutex<HashMap<usize, RtcAudioTrack>>> = Arc::default();

        let out = self.out.clone();
        let id = peer_id.to_string();
        pc.on_ice_candidate(Some(Box::new(move |c: IceCandidate| {
            out.event(
                "peer.ice",
                json!({ "peerId": id, "candidate": c.candidate(), "sdpMid": c.sdp_mid(), "sdpMLineIndex": c.sdp_mline_index() }),
            );
        })));
        let out = self.out.clone();
        let id = peer_id.to_string();
        pc.on_connection_state_change(Some(Box::new(move |s| {
            out.event(
                "peer.state",
                json!({ "peerId": id, "state": state_name(s) }),
            );
        })));
        let out = self.out.clone();
        let id = peer_id.to_string();
        let track_map = tracks.clone();
        let pc_for_index = pc.clone();
        pc.on_track(Some(Box::new(move |ev| {
            let mid = ev.transceiver.mid();
            let index = pc_for_index
                .transceivers()
                .iter()
                .position(|t| t.mid() == mid && mid.is_some());
            let (Some(index), MediaStreamTrack::Audio(track)) = (index, ev.track) else {
                return;
            };
            track_map.lock().insert(index, track);
            out.event("peer.track", json!({ "peerId": id, "index": index }));
        })));

        self.peers.insert(
            peer_id.to_string(),
            Peer {
                pc,
                tracks,
                senders: Vec::new(),
                mic_tracks: Vec::new(),
            },
        );
        Ok(Value::Null)
    }

    pub fn add_recv_only_audio(&mut self, peer_id: &str) -> Result<Value, String> {
        let peer = self.peer(peer_id)?;
        peer.pc
            .add_transceiver_for_media(
                MediaType::Audio,
                RtpTransceiverInit {
                    direction: RtpTransceiverDirection::RecvOnly,
                    stream_ids: vec![],
                    send_encodings: vec![],
                },
            )
            .map_err(err)?;
        Ok(json!(peer.pc.transceivers().len() - 1))
    }

    pub fn attach_mic(
        &mut self,
        peer_id: &str,
        mic_id: &str,
        source: &NativeAudioSource,
        enabled: bool,
    ) -> Result<Value, String> {
        let track = self
            .factory
            .create_audio_track(&format!("chatter-mic-{mic_id}"), source.clone());
        track.set_enabled(enabled);
        let peer = self
            .peers
            .get_mut(peer_id)
            .ok_or_else(|| format!("no peer {peer_id}"))?;
        let sender = peer
            .pc
            .add_track(MediaStreamTrack::Audio(track.clone()), &["chatter"])
            .map_err(err)?;
        peer.senders.push(sender);
        peer.mic_tracks.push((mic_id.to_string(), track));
        Ok(Value::Null)
    }

    /// Mute or unmute a mic on every peer it was attached to.
    pub fn set_mic_enabled(&self, mic_id: &str, on: bool) {
        for peer in self.peers.values() {
            for (id, track) in &peer.mic_tracks {
                if id == mic_id {
                    track.set_enabled(on);
                }
            }
        }
    }

    pub async fn create_offer(&self, peer_id: &str) -> Result<Value, String> {
        let pc = self.peer(peer_id)?.pc.clone();
        let offer = pc.create_offer(offer_options()).await.map_err(err)?;
        Ok(json!(offer.to_string()))
    }

    pub async fn set_description(
        &self,
        peer_id: &str,
        local: bool,
        kind: &str,
        sdp: &str,
    ) -> Result<Value, String> {
        let pc = self.peer(peer_id)?.pc.clone();
        let sdp_type: SdpType = kind.parse().map_err(|_| format!("bad sdp type {kind}"))?;
        let desc = SessionDescription::parse(sdp, sdp_type).map_err(err)?;
        if local {
            pc.set_local_description(desc).await.map_err(err)?;
        } else {
            pc.set_remote_description(desc).await.map_err(err)?;
        }
        Ok(Value::Null)
    }

    pub async fn add_ice(
        &self,
        peer_id: &str,
        candidate: &str,
        mid: &str,
        index: i32,
    ) -> Result<Value, String> {
        let pc = self.peer(peer_id)?.pc.clone();
        let candidate = IceCandidate::parse(mid, index, candidate).map_err(err)?;
        // A browser resolves these quietly too; a stale candidate isn't fatal.
        if let Err(e) = pc.add_ice_candidate(candidate).await {
            log::debug!("add_ice_candidate: {e:?}");
        }
        Ok(Value::Null)
    }

    pub fn restart_ice(&self, peer_id: &str) -> Result<Value, String> {
        self.peer(peer_id)?.pc.restart_ice();
        Ok(Value::Null)
    }

    pub fn set_max_bitrate(&self, peer_id: &str, bps: u64) -> Result<Value, String> {
        for sender in &self.peer(peer_id)?.senders {
            let mut params = sender.parameters();
            if params.encodings.is_empty() {
                params.encodings.push(RtpEncodingParameters::default());
            }
            params.encodings[0].max_bitrate = Some(bps);
            sender.set_parameters(params).map_err(err)?;
        }
        Ok(Value::Null)
    }

    pub fn track(&self, peer_id: &str, index: usize) -> Option<RtcAudioTrack> {
        self.peers.get(peer_id)?.tracks.lock().get(&index).cloned()
    }

    pub async fn stats(&self, peer_id: &str) -> Result<Value, String> {
        let pc = self.peer(peer_id)?.pc.clone();
        let stats = pc.get_stats().await.map_err(err)?;
        Ok(Value::Array(stats.iter().filter_map(w3c_stats).collect()))
    }

    pub fn close(&mut self, peer_id: &str) -> Result<Value, String> {
        if let Some(peer) = self.peers.remove(peer_id) {
            // The callbacks hold clones of the connection; drop them so it
            // can actually go away.
            peer.pc.on_track(None);
            peer.pc.on_ice_candidate(None);
            peer.pc.on_connection_state_change(None);
            peer.pc.close();
        }
        Ok(Value::Null)
    }

    pub fn close_all(&mut self) {
        let ids: Vec<String> = self.peers.keys().cloned().collect();
        for id in ids {
            let _ = self.close(&id);
        }
    }
}

/// The browser's stats report shape, for the fields the client reads
/// (useConnectionStats): codecs, rtp streams, the nominated pair, the mic.
fn w3c_stats(stat: &RtcStats) -> Option<Value> {
    Some(match stat {
        RtcStats::Codec(c) => json!({
            "type": "codec", "id": c.rtc.id, "mimeType": c.codec.mime_type,
            "clockRate": c.codec.clock_rate, "channels": c.codec.channels,
            "payloadType": c.codec.payload_type, "sdpFmtpLine": c.codec.sdp_fmtp_line,
        }),
        RtcStats::InboundRtp(s) => json!({
            "type": "inbound-rtp", "id": s.rtc.id, "kind": s.stream.kind, "codecId": s.stream.codec_id,
            "mid": s.inbound.mid, "packetsReceived": s.received.packets_received,
            "packetsLost": s.received.packets_lost, "jitter": s.received.jitter,
            "bytesReceived": s.inbound.bytes_received,
            "totalSamplesReceived": s.inbound.total_samples_received,
            "concealedSamples": s.inbound.concealed_samples,
            "audioLevel": s.inbound.audio_level, "totalAudioEnergy": s.inbound.total_audio_energy,
        }),
        RtcStats::OutboundRtp(s) => json!({
            "type": "outbound-rtp", "id": s.rtc.id, "kind": s.stream.kind, "codecId": s.stream.codec_id,
            "packetsSent": s.sent.packets_sent, "bytesSent": s.sent.bytes_sent,
        }),
        RtcStats::CandidatePair(p) => json!({
            "type": "candidate-pair", "id": p.rtc.id, "nominated": p.candidate_pair.nominated,
            "currentRoundTripTime": p.candidate_pair.current_round_trip_time,
            "bytesSent": p.candidate_pair.bytes_sent, "bytesReceived": p.candidate_pair.bytes_received,
        }),
        RtcStats::MediaSource(m) => json!({
            "type": "media-source", "id": m.rtc.id, "kind": m.source.kind, "audioLevel": m.audio.audio_level,
        }),
        _ => return None,
    })
}
