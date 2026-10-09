//! Audio devices, named the way the settings dialog shows them.
//!
//! Ids are cpal's `DeviceId` in its `host:id` form (stable across restarts on
//! WASAPI, ALSA and PipeWire). "default" means the system default and follows
//! it when it changes.

use std::sync::{Mutex, MutexGuard};

use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;

/// One host for the engine's lifetime. On Linux every `cpal::default_host()`
/// opens a PulseAudio connection whose reactor thread never exits, even once
/// the host is dropped, so a host per call leaked a connection on every device
/// poll until pipewire-pulse hit its client cap and refused every app.
static HOST: Mutex<Option<cpal::Host>> = Mutex::new(None);

fn host() -> MutexGuard<'static, Option<cpal::Host>> {
    HOST.lock().unwrap_or_else(|e| e.into_inner())
}

/// Forget the host when its server went away (pipewire-pulse restarted), so
/// the next call reconnects. A dead connection's thread has already exited.
fn forget_if_gone(slot: &mut Option<cpal::Host>, err: &cpal::Error) {
    if err.kind() == cpal::ErrorKind::StreamInvalidated {
        *slot = None;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AudioDevice {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceList {
    pub inputs: Vec<AudioDevice>,
    pub outputs: Vec<AudioDevice>,
}

fn describe(device: &cpal::Device) -> Option<AudioDevice> {
    let id = device.id().ok()?.to_string();
    let label = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| device.to_string());
    Some(AudioDevice { id, label })
}

pub fn list() -> DeviceList {
    let mut slot = host();
    let host = slot.get_or_insert_with(cpal::default_host);
    let default = |label: &str| AudioDevice {
        id: "default".into(),
        label: label.into(),
    };
    let mut inputs = vec![default("Default")];
    let mut outputs = vec![default("Default")];
    let found_inputs = host.input_devices();
    let found_outputs = host.output_devices();
    match found_inputs {
        Ok(devices) => inputs.extend(devices.filter_map(|d| describe(&d))),
        Err(e) => forget_if_gone(&mut slot, &e),
    }
    match found_outputs {
        Ok(devices) => outputs.extend(devices.filter_map(|d| describe(&d))),
        Err(e) => forget_if_gone(&mut slot, &e),
    }
    DeviceList { inputs, outputs }
}

/// The device for an id from `list()`, the default when it is "default" or
/// no longer exists (unplugged, or an id saved by the browser's stack).
pub fn input(id: &str) -> Option<cpal::Device> {
    let mut slot = host();
    let host = slot.get_or_insert_with(cpal::default_host);
    by_id(host, id, true).or_else(|| host.default_input_device())
}

pub fn output(id: &str) -> Option<cpal::Device> {
    let mut slot = host();
    let host = slot.get_or_insert_with(cpal::default_host);
    by_id(host, id, false).or_else(|| host.default_output_device())
}

/// The config to open a device with, asking for `ms` of audio per callback.
///
/// On Linux, cpal's PulseAudio backend otherwise leaves the buffer to the
/// server, and its default is about two seconds (PulseAudio and
/// pipewire-pulse alike). The mic then arrives in bursts bigger than the
/// capture ring, which drops the rest, and the speakers run far behind.
/// WASAPI's default period is already 10 ms, so Windows keeps it.
pub fn stream_config(supported: &cpal::SupportedStreamConfig, ms: u32) -> cpal::StreamConfig {
    #[allow(unused_mut)]
    let mut config = supported.config();
    #[cfg(target_os = "linux")]
    if let cpal::SupportedBufferSize::Range { min, max } = *supported.buffer_size() {
        let frames = config.sample_rate * ms / 1000;
        config.buffer_size = cpal::BufferSize::Fixed(frames.clamp(min, max));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = ms;
    config
}

/// The frames per callback a stream ended up with, for the log.
pub fn describe_buffer(stream: &cpal::Stream) -> String {
    use cpal::traits::StreamTrait;
    match stream.buffer_size() {
        Ok(frames) => format!("{frames} frames per callback"),
        Err(_) => "unknown buffer".into(),
    }
}

fn by_id(host: &cpal::Host, id: &str, input: bool) -> Option<cpal::Device> {
    if id == "default" || id.is_empty() {
        return None;
    }
    let parsed: cpal::DeviceId = id.parse().ok()?;
    let device = host.device_by_id(&parsed)?;
    let fits = if input {
        device.supports_input()
    } else {
        device.supports_output()
    };
    fits.then_some(device)
}

/// Report when the device list changes. cpal has no change notification, so
/// this polls; enumeration is cheap.
pub fn watch(on_change: impl Fn() + Send + 'static) {
    std::thread::Builder::new()
        .name("device-watch".into())
        .spawn(move || {
            let mut last = list();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let now = list();
                if now != last {
                    last = now;
                    on_change();
                }
            }
        })
        .expect("device watch thread");
}
