//! Embeddable HTTP/WebSocket library for ESP32 CSI capture.
//!
//! This crate bridges ESP32 boards running the `esp-csi-cli-rs` firmware to HTTP and WebSocket
//! clients. It owns one serial task per attached board, turns JSON request bodies into the
//! firmware's CLI commands, decodes the COBS/postcard CSI frames that come back, and fans them out
//! to WebSocket clients and/or Parquet session dumps. The ready-to-run executable built on it is
//! [`csi-webserver`](https://github.com/csi-rs/csi-webserver-rs); embed this crate instead when you
//! want the server inside your own process.
//!
//! # Getting started
//!
//! ```no_run
//! use std::time::Duration;
//! use csi_webserver_core::{AppState, ServerConfig, SupervisorConfig, run_supervisor, serve};
//!
//! # async fn run() -> std::io::Result<()> {
//! let state = AppState::new();
//! // Discover and attach boards as they are plugged in …
//! tokio::spawn(run_supervisor(SupervisorConfig {
//!     registry: state.devices.clone(),
//!     baud_rate: 115_200,
//!     scan_interval: Duration::from_secs(2),
//!     aliases: vec![],
//! }));
//! // … and serve the API.
//! serve(ServerConfig { bind: "0.0.0.0:3000".into() }, state).await
//! # }
//! ```
//!
//! To attach a board without hotplug, call [`DeviceRegistry::attach`] with a
//! [`DeviceAttachSpec`]; [`probe_port`] reads its USB identity first. To extend the router, start
//! from [`build_router`] and nest or merge your own routes.
//!
//! # Node modes
//!
//! A node is described by four attributes — network role, collection mode, operational mode and
//! session role — defined once, in the
//! [network model](https://github.com/csi-rs/esp-csi-rs/blob/main/docs/network-model.md). Through
//! this API the `mode` of [`WifiConfig`](models::WifiConfig) selects the operational mode (Wi-Fi
//! station, sniffer or access point, an emitter, or an ESP-NOW or ESP-NOW simplex end) and its
//! optional `collection` field selects the collection mode where the mode admits a choice; the
//! host is always the session initiator. Modes this crate does not name are added by an embedder
//! through [`CsiProfile`].
//!
//! # Routes
//!
//! Every per-device route lives under `/api/devices/{id}`, where `{id}` is the id the device was
//! attached with; `GET /api/devices` lists them.
//!
//! | Route | Purpose |
//! |---|---|
//! | `GET  …/config` | cached view of the device configuration |
//! | `POST …/config/reset` | restore firmware defaults |
//! | `POST …/config/wifi` | operational mode, channel, peer and collection mode (`set-wifi`) |
//! | `POST …/config/traffic` | traffic generation rate |
//! | `POST …/config/csi` | CSI acquisition flags and presets |
//! | `POST …/config/csi-output` | runtime gate on off-device CSI delivery |
//! | `POST …/config/output-mode` | host-side sink: `stream`, `dump` or `both` |
//! | `POST …/config/rate`, `…/protocol`, `…/io-tasks`, `…/csi-delivery` | radio and delivery tuning |
//! | `POST …/control/start`, `…/stop`, `…/reset`, `…/stats`; `GET …/control/status` | run control |
//! | `GET  …/info` | firmware identity and CLI protocol check |
//! | `GET  …/ws` | WebSocket stream of CSI frames |
//!
//! The request bodies are in [`models`] and the handlers in [`routes`]. The full HTTP reference is
//! the [`csi-webserver` API.md](https://github.com/csi-rs/csi-webserver-rs/blob/main/API.md).

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
pub use serial::{ExternalDeviceChannels, spawn_external_device};
pub use server::{ServerConfig, build_router, serve};
pub use state::{AppState, DeviceAttachSpec, DeviceHandle, DeviceRegistry};
pub use supervisor::{PortInfo, SupervisorConfig, detect_esp_ports, probe_port, run_supervisor};
