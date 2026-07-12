//! Embeddable HTTP/WebSocket library for ESP32 CSI capture.
//!
//! See the [crate README](https://github.com/csi-rs/csi-webserver-core-rs/blob/main/README.md)
//! for embedding examples. For the HTTP API exposed by the default binary, see
//! [`csi-webserver` API.md](https://github.com/csi-rs/csi-webserver-rs/blob/main/API.md).

pub mod csi;
pub mod models;
pub mod parquet_sink;
pub mod profile;
pub mod routes;
pub mod serial;
pub mod server;
pub mod state;
pub mod supervisor;

pub use profile::{CsiProfile, StandardCsiProfile};
pub use server::{ServerConfig, build_router, serve};
pub use state::{AppState, DeviceAttachSpec, DeviceHandle, DeviceRegistry};
pub use supervisor::{PortInfo, SupervisorConfig, detect_esp_ports, probe_port, run_supervisor};
