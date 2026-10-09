//! Chatter's chat WebSocket, which also carries all WebRTC signalling.

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Cheap to clone; every clone writes to the same socket, in order.
#[derive(Clone)]
pub struct Outbox(mpsc::UnboundedSender<String>);

impl Outbox {
    pub fn send(&self, message: Value) {
        log::trace!("ws -> {message}");
        let _ = self.0.send(message.to_string());
    }
}

/// Connect, authenticate, and wait for the server's `connected` hello.
pub async fn connect(
    url: &str,
    access_token: &str,
) -> Result<(Outbox, mpsc::UnboundedReceiver<Value>)> {
    connect_as(url, access_token, false).await
}

/// As [`connect`], optionally identifying as the desktop app.
pub async fn connect_as(
    url: &str,
    access_token: &str,
    desktop: bool,
) -> Result<(Outbox, mpsc::UnboundedReceiver<Value>)> {
    let (socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .context("websocket connect")?;
    let (mut sink, mut stream) = socket.split();

    // The first frame authenticates; it has no "type".
    let mut hello = json!({ "access_token": access_token, "is_mobile": false });
    if desktop {
        hello["client"] = json!({ "kind": "desktop", "version": env!("CARGO_PKG_VERSION") });
    }
    sink.send(Message::text(hello.to_string())).await?;

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            if sink.send(Message::text(text)).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // Reading keeps the connection alive: tungstenite answers the server's
    // pings (it drops sockets silent for 45 s) as part of polling the stream.
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        while let Some(frame) = stream.next().await {
            match frame {
                Ok(Message::Text(text)) => match serde_json::from_str::<Value>(&text) {
                    Ok(value) => {
                        if in_tx.send(value).is_err() {
                            break;
                        }
                    }
                    Err(e) => log::warn!("unparseable frame: {e}"),
                },
                Ok(Message::Close(frame)) => {
                    log::info!("server closed the socket: {frame:?}");
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    log::warn!("websocket error: {e}");
                    break;
                }
            }
        }
    });

    loop {
        let msg = in_rx.recv().await.context("socket closed before hello")?;
        if let Some(err) = msg.get("error") {
            bail!("server refused the socket: {err}");
        }
        if msg["type"] == "connected" {
            log::info!("connected as {}", msg["user_id"]);
            break;
        }
    }
    Ok((Outbox(out_tx), in_rx))
}

/// The JSON shape the browser sends for an ICE candidate.
pub fn candidate_json(candidate: &str, sdp_mid: &str, sdp_mline_index: i32) -> Value {
    json!({
        "candidate": candidate,
        "sdpMid": sdp_mid,
        "sdpMLineIndex": sdp_mline_index,
        "usernameFragment": Value::Null,
    })
}
