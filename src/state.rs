//! Shared application state used by Axum route handlers.
//!
//! Each attached ESP32 is represented by a [`DeviceHandle`] that owns the
//! channels and runtime flags coordinating route requests with that device's
//! long-running serial background task. [`AppState`] is a thin registry mapping
//! a stable device id to its handle; the device set is mutated at runtime by
//! the hotplug supervisor (see [`crate::supervisor::run_supervisor`]) or via
//! [`DeviceRegistry::attach`].

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::models::{DeviceConfig, DeviceInfo, OutputMode};
use crate::profile::{CsiProfile, StandardCsiProfile};
use crate::serial;

/// Specification for attaching one ESP32 device to the registry.
#[derive(Debug, Clone)]
pub struct DeviceAttachSpec {
    pub id: String,
    pub port_path: String,
    pub baud_rate: u32,
    pub native_usb: bool,
    pub mac: Option<String>,
    /// Carried across handle respawns when a native-USB board re-enumerates.
    pub recovery_cycles: u32,
    /// Declared fault from a prior connection attempt, if any.
    pub fault: Option<String>,
}

impl Default for DeviceAttachSpec {
    fn default() -> Self {
        Self {
            id: String::new(),
            port_path: String::new(),
            baud_rate: 115_200,
            native_usb: false,
            mac: None,
            recovery_cycles: 0,
            fault: None,
        }
    }
}

/// One-shot reply channel for an in-flight `info` exchange.
pub type InfoResponder = oneshot::Sender<Result<DeviceInfo, String>>;

/// All per-device runtime state for one attached ESP32.
///
/// Wrapped in an `Arc` and shared between the device's serial task and every
/// route handler that resolves it from the registry. The inner atomics,
/// mutexes, and watch senders are plain (not individually `Arc`-wrapped): the
/// single `Arc<DeviceHandle>` provides the sharing.
pub struct DeviceHandle {
    /// Stable identifier used in URL paths. Derived from the board's MAC
    /// (`D0-CF-13-E2-90-E8`) when known, so it survives a `ttyACMx`
    /// renumbering; falls back to a CLI alias or the sanitized port basename.
    pub id: String,
    /// Stable hardware identity: the board's MAC as reported by its USB
    /// `iSerialNumber` descriptor (`AA:BB:CC:DD:EE:FF`), read at scan time
    /// without opening the port. `None` for adapters that expose no serial
    /// number (most CP210x/CH340 UART bridges). This is what lets the
    /// supervisor follow a board across a re-enumeration that changes its
    /// `/dev/ttyACMx` number — see [`crate::supervisor::run_supervisor`]. The
    /// firmware also echoes it in the `info` block (`mac=`) as confirmation.
    pub mac: Option<String>,
    /// USB serial port path used to reach the ESP32 (e.g. `/dev/ttyUSB0`).
    /// Pinned for the lifetime of the per-device task; if the board
    /// re-enumerates under a different node the supervisor tears this task
    /// down and respawns it (keyed by [`Self::mac`]) at the new path.
    pub port_path: String,
    /// Baud rate negotiated at startup. The serial task and the RTS-reset
    /// handler both read this so a single source of truth governs the link.
    pub baud_rate: u32,
    /// True when this port is an Espressif native USB-Serial-JTAG endpoint
    /// (VID `0x303A`). Such chips re-enumerate their USB device when reset, so
    /// the serial task must NOT pulse RTS/DTR on connect — doing so drops the
    /// `/dev/ttyACMx` node (often returning under a different number) and the
    /// pinned-path reconnect loop can then never re-verify the device.
    pub native_usb: bool,
    /// Whether the serial task currently has an open and healthy ESP32 link.
    pub serial_connected: AtomicBool,
    /// Best-effort flag: true after successful `start`, false after reset/disconnect.
    pub collection_running: AtomicBool,
    /// `true` once the serial task (or an explicit `/api/.../info` call) has
    /// observed a valid `ESP-CSI-CLI/<version>` magic block from the device.
    /// Cleared on disconnect, on `reset` (until the post-reset re-verification
    /// completes), and on a failed verification. Command endpoints refuse to
    /// send while this is `false`.
    pub firmware_verified: AtomicBool,
    /// Send CLI command strings to the serial background task.
    pub cmd_tx: mpsc::Sender<String>,
    /// Broadcast raw CSI frame bytes (COBS-framed postcard) to this device's
    /// WebSocket clients.
    pub csi_tx: broadcast::Sender<Vec<u8>>,
    /// Notify the serial task of output-mode changes (stream / dump / both).
    pub output_mode_tx: watch::Sender<OutputMode>,
    /// Signal the serial task of the current session's dump file path.
    /// `Some(path)` → open/reuse that file; `None` → session ended, close file.
    pub session_file_tx: watch::Sender<Option<String>>,
    /// Issue an `info` command on the device and capture the magic block.
    /// The serial task synchronously consumes the responder.
    pub info_request_tx: mpsc::Sender<InfoResponder>,
    /// Cached view of this device's configuration.
    pub config: Mutex<DeviceConfig>,
    /// Last successfully parsed firmware identification block. `None` until
    /// the first verification succeeds; cleared alongside `firmware_verified`.
    pub device_info: Mutex<Option<DeviceInfo>>,
    /// Detected chip fault (e.g. the ESP32-C5/C6 USB-JTAG reset-loop wedge or
    /// a ROM boot loop), with the recovery action, set by the serial task when
    /// firmware verification keeps failing with a recognizable boot signature.
    /// Cleared on successful verification. Surfaced via `GET /api/devices`.
    pub fault: Mutex<Option<String>>,
    /// Completed recovery attempts (the firmware `restart` rung, whose reboot
    /// re-enumerates native USB and thus restarts the per-connection
    /// escalation ladder). Carried across handle respawns (re-enumeration
    /// replaces the handle!) via `RecoveryCarryOver` so an unrecoverable
    /// device is restarted at most once and then declared faulted, instead
    /// of being bounced through reboots forever. Cleared on successful
    /// verification.
    pub recovery_cycles: AtomicU32,
    /// Cancelled by the supervisor when the device is unplugged, signalling the
    /// serial task to tear down cleanly.
    pub shutdown: CancellationToken,
    /// Optional capability extensions (extra protocols/presets/format labels).
    /// Inherited from the owning [`DeviceRegistry`]; the per-device serial task
    /// consults it when labelling Parquet `data_format`.
    pub profile: Arc<dyn CsiProfile>,
}

impl DeviceHandle {
    /// Returns an early-return tuple suitable for handlers when the firmware
    /// has not yet been verified as `esp-csi-cli-rs`. Use this to short-circuit
    /// any endpoint that issues a CLI command — sending commands to an
    /// unverified device may interact with whatever bootloader/firmware is
    /// listening in unintended ways.
    pub fn require_firmware(
        &self,
    ) -> Option<(axum::http::StatusCode, axum::Json<crate::models::ApiResponse>)> {
        if self.firmware_verified.load(Ordering::SeqCst) {
            None
        } else {
            Some((
                axum::http::StatusCode::PRECONDITION_FAILED,
                axum::Json(crate::models::ApiResponse {
                    success: false,
                    message:
                        "Firmware not verified as esp-csi-cli-rs. Call GET .../info to verify, \
                         or POST .../control/reset to power-cycle and re-check."
                            .to_string(),
                }),
            ))
        }
    }
}

/// Runtime registry of attached devices, keyed by [`DeviceHandle::id`].
///
/// Uses a `std::sync::RwLock`: every critical section clones an `Arc` out and
/// drops the guard immediately — the lock is never held across an `.await`, so
/// an async lock would only add cost. The supervisor is the only writer.
pub struct DeviceRegistry {
    map: RwLock<HashMap<String, Arc<DeviceHandle>>>,
    /// Capability profile handed to every device spawned by this registry.
    pub profile: Arc<dyn CsiProfile>,
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
            profile: Arc::new(StandardCsiProfile),
        }
    }
}

impl DeviceRegistry {
    /// A registry whose spawned devices carry the given capability profile.
    pub fn with_profile(profile: Arc<dyn CsiProfile>) -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
            profile,
        }
    }

    /// Look up a device by id, returning an owned `Arc` clone.
    pub fn get(&self, id: &str) -> Option<Arc<DeviceHandle>> {
        self.map.read().unwrap().get(id).cloned()
    }

    /// Insert a newly discovered device. Returns the previous handle for this
    /// id, if any.
    pub fn insert(&self, dev: Arc<DeviceHandle>) -> Option<Arc<DeviceHandle>> {
        self.map.write().unwrap().insert(dev.id.clone(), dev)
    }

    /// Remove a device by id, returning its handle if present.
    pub fn remove(&self, id: &str) -> Option<Arc<DeviceHandle>> {
        self.map.write().unwrap().remove(id)
    }

    /// Snapshot of all current devices, sorted by id for stable listings.
    pub fn snapshot(&self) -> Vec<Arc<DeviceHandle>> {
        let mut devices: Vec<Arc<DeviceHandle>> = self.map.read().unwrap().values().cloned().collect();
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        devices
    }

    /// Spawn a serial worker for `spec` and register it. Returns the new handle.
    pub fn attach(&self, spec: DeviceAttachSpec) -> Arc<DeviceHandle> {
        let handle = serial::spawn_device(&spec, self.profile.clone());
        self.insert(handle.clone());
        handle
    }

    /// Remove a device, cancel its serial task, and return its handle.
    pub fn detach(&self, id: &str) -> Option<Arc<DeviceHandle>> {
        let handle = self.remove(id)?;
        handle.shutdown.cancel();
        Some(handle)
    }
}

/// Shared application state, cheaply cloned into every route handler via Axum's
/// `State` extractor. Holds only the device registry; all per-device state
/// lives behind [`DeviceRegistry`].
#[derive(Clone)]
pub struct AppState {
    pub devices: Arc<DeviceRegistry>,
}

impl AppState {
    /// Empty registry with the default no-op [`StandardCsiProfile`]; populate
    /// via [`DeviceRegistry::attach`] or [`crate::supervisor::run_supervisor`].
    pub fn new() -> Self {
        Self {
            devices: Arc::new(DeviceRegistry::default()),
        }
    }

    /// Empty registry whose devices carry a custom capability [`CsiProfile`].
    /// An embedder supplies its own profile to extend the accepted protocols,
    /// CSI presets, and `data_format` labels.
    pub fn with_profile(profile: Arc<dyn CsiProfile>) -> Self {
        Self {
            devices: Arc::new(DeviceRegistry::with_profile(profile)),
        }
    }

    /// The capability profile shared by this state's devices.
    pub fn profile(&self) -> &Arc<dyn CsiProfile> {
        &self.devices.profile
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
