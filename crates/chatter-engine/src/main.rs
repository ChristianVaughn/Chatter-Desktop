//! chatter-engine: the Chatter desktop app's native media process.
//!
//! The app starts it and talks to it over stdin/stdout (see protocol.rs and
//! apps/desktop/src/shared/engine.ts). It owns voice — peer connections on
//! Google libwebrtc, the microphone pipeline, the speakers — plus device
//! enumeration and system-wide push-to-talk. It logs to stderr, which the app
//! forwards to its own log.

mod capture;
mod devices;
mod engine;
mod fake;
mod peers;
mod playout;
mod processes;
mod protocol;
mod resample;

use protocol::Incoming;

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,libwebrtc=warn"),
    )
    .target(env_logger::Target::Stderr)
    .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");

    runtime.block_on(async {
        let out = protocol::writer();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        protocol::reader(tx);

        let mut engine = engine::Engine::new(out.clone());
        out.event("hello", engine.hello());
        log::info!("chatter-engine {} ready", env!("CARGO_PKG_VERSION"));

        let watch_out = out.clone();
        devices::watch(move || watch_out.event("devices.changed", serde_json::json!({})));

        // One request at a time, in arrival order: the app depends on a peer
        // existing before its transceivers, and those before the offer.
        while let Some(incoming) = rx.recv().await {
            match incoming {
                // A full process scan takes a while and touches nothing else,
                // so it runs beside the loop rather than holding up a call.
                Incoming::Request(req) if req.op == "processes.list" => {
                    let out = out.clone();
                    tokio::task::spawn_blocking(move || {
                        let list =
                            serde_json::to_value(processes::list()).map_err(|e| e.to_string());
                        out.respond(req.id, list);
                    });
                }
                Incoming::Request(req) => {
                    let result = engine.handle(&req).await;
                    if let Err(e) = &result {
                        log::warn!("{} failed: {e}", req.op);
                    }
                    out.respond(req.id, result);
                }
                Incoming::Binary { stream, data } => {
                    log::debug!(
                        "unexpected binary frame on stream {stream} ({} bytes)",
                        data.len()
                    );
                }
            }
        }
        // The app closed our stdin: put things back on the way out.
        drop(engine);
        log::info!("chatter-engine stopped");
    });
    // Capture and playout threads would otherwise keep the process alive.
    std::process::exit(0);
}
