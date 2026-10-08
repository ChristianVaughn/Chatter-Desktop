//! Port of the client's voice SDP munging (Chatter `client/src/lib/webrtc.ts`,
//! `mungeVoiceAudioSdp` and `clampVoiceBitrate`). It is applied to the
//! server's publish *answer* before `setRemoteDescription`, exactly as the
//! browser does, and the tests mirror `voiceBitrate.test.ts`.

use regex::Regex;

pub const VOICE_BITRATE_MIN_BPS: u32 = 8_000;
pub const VOICE_BITRATE_MAX_BPS: u32 = 256_000;
pub const VOICE_BITRATE_DEFAULT_BPS: u32 = 32_000;
/// The SFU derives its frame size from this (`src/backend/constants.rs`).
pub const VOICE_PTIME_MS: u32 = 40;
pub const VOICE_MAXPTIME_MS: u32 = 120;

pub fn clamp_voice_bitrate(bps: Option<f64>) -> u32 {
    match bps {
        Some(b) if b.is_finite() => (b.round() as i64)
            .clamp(VOICE_BITRATE_MIN_BPS as i64, VOICE_BITRATE_MAX_BPS as i64)
            as u32,
        _ => VOICE_BITRATE_DEFAULT_BPS,
    }
}

pub fn munge_voice_audio_sdp(sdp: &str, bitrate_bps: u32) -> String {
    let opus = Regex::new(r"(?i)a=rtpmap:(\d+) opus/48000").unwrap();
    let Some(caps) = opus.captures(sdp) else {
        return sdp.to_string();
    };
    let pt = caps[1].to_string();
    let bitrate = clamp_voice_bitrate(Some(bitrate_bps as f64));

    let existing_re = Regex::new(&format!(r"a=fmtp:{pt} [^\r\n]+")).unwrap();
    let existing = existing_re.find(sdp).map(|m| m.as_str().to_string());
    let prefix = format!("a=fmtp:{pt} ");
    let base = existing
        .as_deref()
        .map(|line| line[prefix.len()..].to_string())
        .unwrap_or_else(|| "minptime=10;useinbandfec=1".to_string());
    let mut params: Vec<String> = base
        .split(';')
        .map(str::trim)
        .filter(|p| {
            let lower = p.to_ascii_lowercase();
            !p.is_empty()
                && !lower.starts_with("maxaveragebitrate=")
                && !lower.starts_with("usedtx=")
        })
        .map(String::from)
        .collect();
    params.push(format!("maxaveragebitrate={bitrate}"));
    params.push("usedtx=1".to_string());
    let line = format!("a=fmtp:{pt} {}", params.join(";"));

    let with_fmtp = if existing.is_some() {
        existing_re
            .replace(sdp, regex::NoExpand(&line))
            .into_owned()
    } else {
        let rtpmap = Regex::new(&format!(r"(a=rtpmap:{pt} opus/48000[^\r\n]*\r?\n)")).unwrap();
        rtpmap
            .replace(sdp, |c: &regex::Captures| format!("{}{}\r\n", &c[1], line))
            .into_owned()
    };
    with_voice_ptime(&with_fmtp, &line)
}

fn with_voice_ptime(sdp: &str, after_line: &str) -> String {
    let stripped = Regex::new(r"\r?\na=(?:max)?ptime:\d+")
        .unwrap()
        .replace_all(sdp, "")
        .into_owned();
    let ptime_lines = format!("\r\na=ptime:{VOICE_PTIME_MS}\r\na=maxptime:{VOICE_MAXPTIME_MS}");
    match stripped.find(after_line) {
        Some(anchor) => {
            let at = anchor + after_line.len();
            format!("{}{}{}", &stripped[..at], ptime_lines, &stripped[at..])
        }
        None => stripped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SDP_WITH_FMTP: &str = "v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=1\r\n";

    #[test]
    fn clamps_bitrate() {
        assert_eq!(clamp_voice_bitrate(None), VOICE_BITRATE_DEFAULT_BPS);
        assert_eq!(
            clamp_voice_bitrate(Some(f64::NAN)),
            VOICE_BITRATE_DEFAULT_BPS
        );
        assert_eq!(clamp_voice_bitrate(Some(1_000.0)), VOICE_BITRATE_MIN_BPS);
        assert_eq!(
            clamp_voice_bitrate(Some(1_000_000.0)),
            VOICE_BITRATE_MAX_BPS
        );
        assert_eq!(clamp_voice_bitrate(Some(96_000.0)), 96_000);
    }

    #[test]
    fn sets_bitrate_keeping_params() {
        let out = munge_voice_audio_sdp(SDP_WITH_FMTP, 128_000);
        assert!(out.contains("a=fmtp:111 minptime=10;useinbandfec=1;maxaveragebitrate=128000"));
    }

    #[test]
    fn replaces_existing_bitrate() {
        let sdp = SDP_WITH_FMTP.replace(
            "a=fmtp:111 minptime=10;useinbandfec=1",
            "a=fmtp:111 minptime=10;useinbandfec=1;maxaveragebitrate=24000",
        );
        let out = munge_voice_audio_sdp(&sdp, 64_000);
        assert!(out.contains("a=fmtp:111 minptime=10;useinbandfec=1;maxaveragebitrate=64000"));
        assert!(!out.contains("maxaveragebitrate=24000"));
    }

    #[test]
    fn inserts_fmtp_when_missing() {
        let sdp = SDP_WITH_FMTP.replace("a=fmtp:111 minptime=10;useinbandfec=1\r\n", "");
        let out = munge_voice_audio_sdp(&sdp, 8_000);
        assert!(out.contains("a=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=1;maxaveragebitrate=8000"));
    }

    #[test]
    fn clamps_inside_munge() {
        assert!(munge_voice_audio_sdp(SDP_WITH_FMTP, 900_000)
            .contains(&format!("maxaveragebitrate={VOICE_BITRATE_MAX_BPS}")));
        assert!(munge_voice_audio_sdp(SDP_WITH_FMTP, 100)
            .contains(&format!("maxaveragebitrate={VOICE_BITRATE_MIN_BPS}")));
    }

    #[test]
    fn forces_single_usedtx() {
        let sdp = SDP_WITH_FMTP.replace(
            "a=fmtp:111 minptime=10;useinbandfec=1",
            "a=fmtp:111 minptime=10;useinbandfec=1;usedtx=0",
        );
        let out = munge_voice_audio_sdp(&sdp, 64_000);
        assert!(out.contains("usedtx=1"));
        assert!(!out.contains("usedtx=0"));
        assert_eq!(out.matches("usedtx=").count(), 1);
    }

    #[test]
    fn leaves_non_opus_alone() {
        let sdp = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 VP8/90000\r\n";
        assert_eq!(munge_voice_audio_sdp(sdp, 64_000), sdp);
    }

    #[test]
    fn sets_single_ptime_after_fmtp() {
        let sdp = SDP_WITH_FMTP.replace(
            "a=fmtp:111 minptime=10;useinbandfec=1\r\n",
            "a=fmtp:111 minptime=10;useinbandfec=1\r\na=ptime:20\r\na=maxptime:60\r\n",
        );
        let out = munge_voice_audio_sdp(&sdp, VOICE_BITRATE_DEFAULT_BPS);
        assert!(!out.contains("a=ptime:20"));
        assert!(!out.contains("a=maxptime:60"));
        assert_eq!(out.matches("a=ptime:").count(), 1);
        assert_eq!(out.matches("a=maxptime:").count(), 1);
        let lines: Vec<&str> = out.split("\r\n").collect();
        let fmtp = lines
            .iter()
            .position(|l| l.starts_with("a=fmtp:111"))
            .unwrap();
        assert_eq!(
            lines.iter().position(|l| *l == "a=ptime:40").unwrap(),
            fmtp + 1
        );
        assert_eq!(
            lines.iter().position(|l| *l == "a=maxptime:120").unwrap(),
            fmtp + 2
        );
    }
}
