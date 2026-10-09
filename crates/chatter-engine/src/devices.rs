//! Audio devices, named the way the settings dialog shows them.
//!
//! Ids are cpal's `DeviceId` in its `host:id` form (stable across restarts on
//! WASAPI, ALSA and PipeWire). "default" means the system default and follows
//! it when it changes.

use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;

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
    let host = cpal::default_host();
    let default = |label: &str| AudioDevice {
        id: "default".into(),
        label: label.into(),
    };
    let mut inputs = vec![default("Default")];
    let mut outputs = vec![default("Default")];
    if let Ok(devices) = host.input_devices() {
        inputs.extend(devices.filter_map(|d| describe(&d)));
    }
    if let Ok(devices) = host.output_devices() {
        outputs.extend(devices.filter_map(|d| describe(&d)));
    }
    DeviceList { inputs, outputs }
}

/// The device for an id from `list()`, the default when it is "default" or
/// no longer exists (unplugged, or an id saved by the browser's stack).
pub fn input(id: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    by_id(&host, id, true).or_else(|| host.default_input_device())
}

pub fn output(id: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    by_id(&host, id, false).or_else(|| host.default_output_device())
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
