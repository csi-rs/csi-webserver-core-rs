//! USB hotplug supervisor and port discovery.
//!
//! Polls attached ESP32 serial ports and registers or removes devices in the
//! shared [`DeviceRegistry`](crate::state::DeviceRegistry).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::time::{Duration, sleep};
use tokio_serial::SerialPortType;

use crate::state::{DeviceAttachSpec, DeviceHandle, DeviceRegistry};

/// Espressif's USB vendor id, used by the built-in USB-Serial-JTAG controller
/// on ESP32-S3 / C3 / C6.
pub const ESPRESSIF_NATIVE_USB_VID: u16 = 0x303A;

/// Known ESP32 USB-UART adapter Vendor IDs.
const ESP_USB_VIDS: &[u16] = &[
    0x10C4,                    // Silicon Labs CP210x
    0x1A86,                    // WCH CH340 / CH341
    ESPRESSIF_NATIVE_USB_VID, // Espressif built-in USB
];

/// Metadata about a serial port, read from USB enumeration without opening it.
#[derive(Debug, Clone)]
pub struct PortInfo {
    pub port_path: String,
    pub native_usb: bool,
    pub mac: Option<String>,
}

/// Probe one port path against the current USB enumeration.
pub fn probe_port(port_path: &str) -> Option<PortInfo> {
    let all_ports = tokio_serial::available_ports().ok()?;
    let info = all_ports.iter().find(|p| p.port_name == port_path)?;
    let native_usb = matches!(
        info.port_type,
        SerialPortType::UsbPort(ref usb) if usb.vid == ESPRESSIF_NATIVE_USB_VID
    );
    let mac = match &info.port_type {
        SerialPortType::UsbPort(usb) => usb.serial_number.clone(),
        _ => None,
    };
    Some(PortInfo {
        port_path: port_path.to_string(),
        native_usb,
        mac,
    })
}

/// Configuration for the hotplug supervisor loop.
pub struct SupervisorConfig {
    pub registry: Arc<DeviceRegistry>,
    pub baud_rate: u32,
    pub scan_interval: Duration,
    pub aliases: Vec<(String, String)>,
}

/// Recovery state carried across a device-handle respawn. Re-enumeration to a
/// new port path tears the handle down and spawns a fresh one — but the
/// destructive recovery rungs themselves cause that re-enumeration, so their
/// once-per-device caps and any declared fault must survive the respawn.
#[derive(Default)]
pub struct RecoveryCarryOver {
    pub recovery_cycles: u32,
    pub fault: Option<String>,
}

impl RecoveryCarryOver {
    /// Snapshot the carry-over state from a handle about to be torn down.
    pub async fn from_handle(dev: &DeviceHandle) -> Self {
        Self {
            recovery_cycles: dev.recovery_cycles.load(Ordering::SeqCst),
            fault: dev.fault.lock().await.clone(),
        }
    }
}

/// Detect *all* available ESP32 USB serial port paths, sorted so device-id
/// assignment is deterministic across scans.
pub fn detect_esp_ports() -> Vec<String> {
    if let Ok(port) = std::env::var("CSI_SERIAL_PORT") {
        tracing::debug!("Using CSI_SERIAL_PORT override: {port}");
        return vec![port];
    }

    let ports = match tokio_serial::available_ports() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to enumerate serial ports: {e}");
            return Vec::new();
        }
    };

    let mut matched: Vec<String> = Vec::new();
    for port in &ports {
        if let SerialPortType::UsbPort(ref info) = port.port_type {
            let name_ok = port.port_name.contains("usbserial")
                || port.port_name.contains("usbmodem")
                || port.port_name.contains("ttyUSB")
                || port.port_name.contains("ttyACM");

            let vid_ok = ESP_USB_VIDS.contains(&info.vid);

            if name_ok || vid_ok {
                matched.push(port.port_name.clone());
            }
        }
    }

    if matched.is_empty() {
        let usb: Vec<&tokio_serial::SerialPortInfo> = ports
            .iter()
            .filter(|p| matches!(p.port_type, SerialPortType::UsbPort(_)))
            .collect();
        if usb.len() == 1 {
            tracing::warn!(
                "No known ESP port found — using the only USB port: {}",
                usb[0].port_name
            );
            matched.push(usb[0].port_name.clone());
        }
    }

    matched.sort();
    matched
}

fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn device_id(port_path: &str, mac: Option<&str>, aliases: &[(String, String)]) -> String {
    for (alias, key) in aliases {
        if key == port_path || Some(key.as_str()) == mac {
            return alias.clone();
        }
    }
    if let Some(mac) = mac {
        return sanitize_id(mac);
    }
    sanitize_id(port_path.rsplit('/').next().unwrap_or(port_path))
}

struct PortCandidate {
    id: String,
    path: String,
    native_usb: bool,
    mac: Option<String>,
}

fn scan_ports(aliases: &[(String, String)]) -> (Vec<PortCandidate>, HashSet<String>) {
    let detected = detect_esp_ports();
    let all_ports = tokio_serial::available_ports().unwrap_or_default();
    let existing: HashSet<String> = all_ports.iter().map(|p| p.port_name.clone()).collect();

    let is_native = |path: &str| {
        all_ports.iter().any(|p| {
            p.port_name == path
                && matches!(
                    p.port_type,
                    SerialPortType::UsbPort(ref info) if info.vid == ESPRESSIF_NATIVE_USB_VID
                )
        })
    };

    let mac_of = |path: &str| -> Option<String> {
        all_ports.iter().find_map(|p| match &p.port_type {
            SerialPortType::UsbPort(info) if p.port_name == path => info.serial_number.clone(),
            _ => None,
        })
    };

    let mut candidates: Vec<PortCandidate> = detected
        .into_iter()
        .map(|path| {
            let mac = mac_of(&path);
            PortCandidate {
                id: device_id(&path, mac.as_deref(), aliases),
                native_usb: is_native(&path),
                mac,
                path,
            }
        })
        .collect();

    for (alias, path) in aliases {
        if existing.contains(path) && !candidates.iter().any(|c| &c.path == path) {
            candidates.push(PortCandidate {
                id: alias.clone(),
                native_usb: is_native(path),
                mac: mac_of(path),
                path: path.clone(),
            });
        }
    }

    (candidates, existing)
}

fn attach_candidate(registry: &DeviceRegistry, c: &PortCandidate, baud: u32, carry: RecoveryCarryOver) {
    registry.attach(DeviceAttachSpec {
        id: c.id.clone(),
        port_path: c.path.clone(),
        baud_rate: baud,
        native_usb: c.native_usb,
        mac: c.mac.clone(),
        recovery_cycles: carry.recovery_cycles,
        fault: carry.fault,
    });
}

/// Hotplug supervisor: the single authority on which devices exist.
pub async fn run_supervisor(config: SupervisorConfig) {
    const DEBOUNCE: u32 = 3;

    let SupervisorConfig {
        registry,
        baud_rate,
        scan_interval,
        aliases,
    } = config;

    let mut missing: HashMap<String, u32> = HashMap::new();

    loop {
        let aliases_scan = aliases.clone();
        let (candidates, _existing) =
            match tokio::task::spawn_blocking(move || scan_ports(&aliases_scan)).await {
                Ok(scan) => scan,
                Err(e) => {
                    tracing::error!("Port enumeration task failed: {e}");
                    sleep(scan_interval).await;
                    continue;
                }
            };

        let present_ids: HashSet<&str> = candidates.iter().map(|c| c.id.as_str()).collect();

        for c in &candidates {
            missing.remove(&c.id);
            match registry.get(&c.id) {
                None => {
                    tracing::info!("Device added: {} ({})", c.id, c.path);
                    attach_candidate(&registry, c, baud_rate, RecoveryCarryOver::default());
                }
                Some(dev) if dev.port_path != c.path => {
                    tracing::info!(
                        "Device {} re-enumerated: {} → {} (following by MAC)",
                        c.id,
                        dev.port_path,
                        c.path,
                    );
                    let carry = RecoveryCarryOver::from_handle(&dev).await;
                    dev.shutdown.cancel();
                    attach_candidate(&registry, c, baud_rate, carry);
                }
                Some(_) => {}
            }
        }

        for dev in registry.snapshot() {
            if present_ids.contains(dev.id.as_str()) {
                missing.remove(&dev.id);
                continue;
            }
            let count = missing.entry(dev.id.clone()).or_insert(0);
            *count += 1;
            if *count >= DEBOUNCE {
                tracing::info!("Device removed: {} ({})", dev.id, dev.port_path);
                registry.detach(&dev.id);
                missing.remove(&dev.id);
            }
        }

        sleep(scan_interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_id_replaces_slashes() {
        assert_eq!(sanitize_id("/dev/ttyUSB0"), "-dev-ttyUSB0");
        assert_eq!(sanitize_id("D0:CF:13:E2:90:E8"), "D0-CF-13-E2-90-E8");
    }

    #[test]
    fn device_id_prefers_mac_over_path() {
        let id = device_id("/dev/ttyACM0", Some("AA:BB:CC:DD:EE:FF"), &[]);
        assert_eq!(id, "AA-BB-CC-DD-EE-FF");
    }

    #[test]
    fn device_id_honours_alias_by_mac() {
        let aliases = vec![("lab1".into(), "AA:BB:CC:DD:EE:FF".into())];
        let id = device_id("/dev/ttyACM0", Some("AA:BB:CC:DD:EE:FF"), &aliases);
        assert_eq!(id, "lab1");
    }
}
