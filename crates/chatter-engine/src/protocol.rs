//! Framing over stdin/stdout, shared with apps/desktop/src/shared/engine.ts:
//!
//! `u32 LE length | u8 kind | payload`, kind `J` (JSON) or `B` (binary: u32 LE
//! stream id, then bytes). Requests carry `id` and `op`; every request gets
//! exactly one response carrying `re`. Events carry `ev`.

use crossbeam_channel::{Receiver, Sender};
use serde_json::{json, Value};
use std::io::{Read, Write};

pub const FRAME_JSON: u8 = b'J';
pub const FRAME_BINARY: u8 = b'B';

#[derive(Debug)]
pub struct Request {
    pub id: u64,
    pub op: String,
    pub args: Value,
}

pub enum Incoming {
    Request(Request),
    Binary { stream: u32, data: Vec<u8> },
}

/// Everything written to stdout goes through one of these, from any thread.
#[derive(Clone)]
pub struct Out(Sender<Vec<u8>>);

impl Out {
    fn frame(&self, kind: u8, payload: &[u8]) {
        let mut buf = Vec::with_capacity(payload.len() + 5);
        buf.extend_from_slice(&((payload.len() + 1) as u32).to_le_bytes());
        buf.push(kind);
        buf.extend_from_slice(payload);
        let _ = self.0.send(buf);
    }

    pub fn json(&self, value: &Value) {
        self.frame(FRAME_JSON, value.to_string().as_bytes());
    }

    pub fn event(&self, name: &str, mut fields: Value) {
        if let Value::Object(map) = &mut fields {
            map.insert("ev".into(), Value::String(name.into()));
        }
        self.json(&fields);
    }

    pub fn respond(&self, id: u64, result: Result<Value, String>) {
        match result {
            Ok(value) => self.json(&json!({ "re": id, "ok": true, "value": value })),
            Err(error) => self.json(&json!({ "re": id, "ok": false, "error": error })),
        }
    }

    pub fn binary(&self, stream: u32, data: &[u8]) {
        let mut payload = Vec::with_capacity(data.len() + 4);
        payload.extend_from_slice(&stream.to_le_bytes());
        payload.extend_from_slice(data);
        self.frame(FRAME_BINARY, &payload);
    }
}

/// Start the stdout writer thread.
pub fn writer() -> Out {
    let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("stdout".into())
        .spawn(move || {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            for buf in rx {
                if out.write_all(&buf).and_then(|_| out.flush()).is_err() {
                    break;
                }
            }
        })
        .expect("stdout thread");
    Out(tx)
}

/// Read frames from stdin until it closes (the app went away).
pub fn reader(tx: tokio::sync::mpsc::UnboundedSender<Incoming>) {
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            let mut len = [0u8; 4];
            loop {
                if input.read_exact(&mut len).is_err() {
                    break;
                }
                let n = u32::from_le_bytes(len) as usize;
                if n == 0 || n > 64 * 1024 * 1024 {
                    log::error!("bad frame length {n}");
                    break;
                }
                let mut frame = vec![0u8; n];
                if input.read_exact(&mut frame).is_err() {
                    break;
                }
                let incoming = match frame[0] {
                    FRAME_JSON => match serde_json::from_slice::<Value>(&frame[1..]) {
                        Ok(mut value) => {
                            let id = value.get("id").and_then(Value::as_u64).unwrap_or(0);
                            let op = value
                                .get("op")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            if let Value::Object(map) = &mut value {
                                map.remove("id");
                                map.remove("op");
                            }
                            Incoming::Request(Request {
                                id,
                                op,
                                args: value,
                            })
                        }
                        Err(e) => {
                            log::warn!("unparseable request: {e}");
                            continue;
                        }
                    },
                    FRAME_BINARY if frame.len() >= 5 => Incoming::Binary {
                        stream: u32::from_le_bytes([frame[1], frame[2], frame[3], frame[4]]),
                        data: frame[5..].to_vec(),
                    },
                    other => {
                        log::warn!("unknown frame kind {other}");
                        continue;
                    }
                };
                if tx.send(incoming).is_err() {
                    break;
                }
            }
            // Dropping `tx` ends the request loop; main then drops the engine,
            // which puts back anything it changed (ducked volumes) and exits.
            log::info!("stdin closed; shutting down");
        })
        .expect("stdin thread");
}
