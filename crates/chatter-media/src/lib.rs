//! Chatter's native media layer.
//!
//! A thin facade over Google libwebrtc, reached through LiveKit's `libwebrtc`
//! bindings (used standalone; no LiveKit server is involved). Everything that
//! knows about libwebrtc's API goes through this crate so a binding upgrade
//! touches one place.

pub use libwebrtc;

use libwebrtc::prelude::{IceServer, RtcConfiguration};
use serde::Deserialize;

/// Opus sample rate everywhere in the pipeline.
pub const SAMPLE_RATE: u32 = 48_000;
/// libwebrtc moves audio in 10 ms frames.
pub const FRAME_MS: u32 = 10;
pub const SAMPLES_PER_FRAME: usize = (SAMPLE_RATE / 1000 * FRAME_MS) as usize;

/// One entry of `GET /api/ice-servers`, shaped like the browser's
/// `RTCIceServer` (`urls` may be a string or a list).
#[derive(Debug, Clone, Deserialize)]
pub struct IceServerJson {
    pub urls: Urls,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub credential: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Urls {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct IceServersResponse {
    #[serde(rename = "iceServers")]
    pub ice_servers: Vec<IceServerJson>,
}

/// Build a peer-connection configuration from the server's ICE list.
pub fn rtc_configuration(servers: &[IceServerJson]) -> RtcConfiguration {
    let mut config = RtcConfiguration::default();
    config.ice_servers = servers
        .iter()
        .map(|s| IceServer {
            urls: match &s.urls {
                Urls::One(u) => vec![u.clone()],
                Urls::Many(u) => u.clone(),
            },
            username: s.username.clone().unwrap_or_default(),
            password: s.credential.clone().unwrap_or_default(),
        })
        .collect();
    config
}
