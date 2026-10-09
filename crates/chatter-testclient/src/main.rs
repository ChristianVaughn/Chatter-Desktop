//! Headless Chatter client used to prove Google libwebrtc (via LiveKit's
//! Rust bindings) can talk to Chatter's SFU exactly like a browser does.
//! See docs/SPIKE-RESULTS.md.

mod api;
mod sdp;
mod signaling;
mod voice;

use anyhow::{bail, Context, Result};
use api::{StoredUser, UserStore};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(about = "Headless Chatter voice client (native media spike)")]
struct Cli {
    /// Chatter server origin.
    #[arg(
        long,
        global = true,
        env = "CHATTER_SERVER",
        default_value = "http://localhost:8000"
    )]
    server: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a test account (TOTP enrolment included) and remember it in .testclient/.
    Register {
        username: String,
        /// Generated when omitted.
        #[arg(long)]
        password: Option<String>,
    },
    /// Print the current TOTP code for a remembered account.
    Totp { username: String },
    /// Log in and print a short-lived access token (for driving a browser test).
    Token { username: String },
    /// Open a spare WebSocket as this user, hold it, then close it — as a
    /// second tab or device would.
    Connect {
        username: String,
        #[arg(long, default_value_t = 3)]
        seconds: u64,
        /// Identify as the desktop app in the first frame.
        #[arg(long)]
        desktop: bool,
    },
    /// Create a room owned by the first user, join the others, and print the
    /// room and its voice channel.
    SetupRoom {
        #[arg(long, default_value = "Native voice spike")]
        name: String,
        #[arg(required = true)]
        usernames: Vec<String>,
    },
    /// Join a voice channel, publish audio, record every slot, report.
    Voice {
        username: String,
        #[arg(long)]
        room: String,
        /// Defaults to the room's first voice channel.
        #[arg(long)]
        channel: Option<String>,
        /// WAV file to loop as the microphone; a pulsed 440 Hz tone otherwise.
        #[arg(long)]
        wav: Option<PathBuf>,
        /// Pitch of the generated tone.
        #[arg(long, default_value_t = 440.0)]
        tone_hz: f32,
        #[arg(long, default_value_t = 30)]
        seconds: u64,
        #[arg(long, default_value = "recordings")]
        out: PathBuf,
    },
}

fn stored(store: &UserStore, server: &str, username: &str) -> Result<StoredUser> {
    store
        .get(server, username)
        .cloned()
        .with_context(|| format!("no stored account {username} for {server}; run `register` first"))
}

fn random_password() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("spike-{:x}{:x}", seed, std::process::id())
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,libwebrtc=warn"),
    )
    .init();
    let cli = Cli::parse();
    let server = url::Url::parse(&cli.server)?.origin().ascii_serialization();
    let mut store = UserStore::load()?;

    match cli.command {
        Command::Register { username, password } => {
            let password = password.unwrap_or_else(random_password);
            let user = api::register(&server, &username, &password).await?;
            store.put(&server, &username, user);
            store.save()?;
            println!("registered {username}; credentials saved in .testclient/users.json");
        }
        Command::Totp { username } => {
            println!(
                "{}",
                api::totp_now(&stored(&store, &server, &username)?.totp_secret)?
            );
        }
        Command::Token { username } => {
            println!(
                "{}",
                api::login(&server, &username, &stored(&store, &server, &username)?)
                    .await?
                    .access_token
            );
        }
        Command::Connect { username, seconds, desktop } => {
            let session = api::login(&server, &username, &stored(&store, &server, &username)?).await?;
            let (_outbox, _inbox) = signaling::connect_as(&session.ws_url()?, &session.access_token, desktop).await?;
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            println!("closed the spare connection for {username}");
        }
        Command::SetupRoom { name, usernames } => {
            let owner = api::login(
                &server,
                &usernames[0],
                &stored(&store, &server, &usernames[0])?,
            )
            .await?;
            let room_id = owner.create_room(&name).await?;
            for username in &usernames[1..] {
                let member =
                    api::login(&server, username, &stored(&store, &server, username)?).await?;
                member.join_room(&room_id).await?;
            }
            let voice = owner
                .channels(&room_id)
                .await?
                .into_iter()
                .find(|c| c.channel_type == "voice")
                .context("room has no voice channel")?;
            println!("room_id={room_id}");
            println!("voice_channel_id={} ({})", voice.channel_id, voice.name);
        }
        Command::Voice {
            username,
            room,
            channel,
            wav,
            tone_hz,
            seconds,
            out,
        } => {
            let session =
                api::login(&server, &username, &stored(&store, &server, &username)?).await?;
            let channels = session.channels(&room).await?;
            let chosen = match channel {
                Some(id) => channels
                    .into_iter()
                    .find(|c| c.channel_id == id)
                    .context("channel not in room")?,
                None => channels
                    .into_iter()
                    .find(|c| c.channel_type == "voice")
                    .context("room has no voice channel")?,
            };
            if chosen.channel_type != "voice" {
                bail!("{} is a {} channel", chosen.channel_id, chosen.channel_type);
            }
            let bitrate = sdp::clamp_voice_bitrate(chosen.voice_bitrate);
            log::info!(
                "joining {} ({}) at {} bps as {}",
                chosen.name,
                chosen.channel_id,
                bitrate,
                session.user_id
            );

            let report = voice::run(
                &session,
                voice::VoiceOptions {
                    room_id: room,
                    channel_id: chosen.channel_id,
                    bitrate,
                    duration: Duration::from_secs(seconds),
                    input: match wav {
                        Some(path) => voice::AudioInput::Wav(path),
                        None => voice::AudioInput::Tone { hz: tone_hz },
                    },
                    out_dir: out,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}
