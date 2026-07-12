# csi-webserver-core

Embeddable Rust library for bridging ESP32 CSI firmware (`esp-csi-cli-rs`) to
HTTP and WebSocket clients. Use this crate when you want to run the CSI server
inside your own process, register devices programmatically, or build a custom
host application.

For the ready-to-run executable, see [`csi-webserver`](https://github.com/csi-rs/csi-webserver-rs/blob/main/README.md).

## Documentation

- Rust API reference: <https://docs.rs/csi-webserver-core>
- HTTP route handlers and request types: [`src/routes/`](src/routes/), [`src/models.rs`](src/models.rs)
- This guide: embedding, supervisor, and registration

## Features

- Axum HTTP API under `/api/devices/{id}/...`
- Per-device WebSocket CSI frame stream (`/ws`)
- Parquet session dumps (decoded from serialized COBS+postcard frames)
- USB hotplug supervisor ([`supervisor`](src/supervisor.rs))
- Explicit device registration ([`DeviceRegistry::attach`](src/state.rs))

## Quick embed

```toml
[dependencies]
csi-webserver-core = "0.1.0"
tokio = { version = "1", features = ["full"] }
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

```rust
use std::time::Duration;
use csi_webserver_core::{AppState, ServerConfig, SupervisorConfig, run_supervisor, serve};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt::init();

    let state = AppState::new();
    tokio::spawn(run_supervisor(SupervisorConfig {
        registry: state.devices.clone(),
        baud_rate: 115_200,
        scan_interval: Duration::from_secs(2),
        aliases: vec![],
    }));

    serve(ServerConfig { bind: "0.0.0.0:3000".into() }, state).await
}
```

## Register a device without hotplug

```rust
use csi_webserver_core::{AppState, DeviceAttachSpec};

let state = AppState::new();
state.devices.attach(DeviceAttachSpec {
    id: "lab1".into(),
    port_path: "/dev/ttyUSB0".into(),
    baud_rate: 115_200,
    native_usb: false,
    mac: None,
    ..Default::default()
});
```

Use [`supervisor::probe_port`](src/supervisor.rs) to read `native_usb` and MAC
from USB enumeration before attaching.

## Public API surface

| Export | Purpose |
|--------|---------|
| `AppState`, `DeviceRegistry`, `DeviceHandle`, `DeviceAttachSpec` | Shared runtime state |
| `ServerConfig`, `build_router`, `serve` | HTTP server |
| `SupervisorConfig`, `run_supervisor`, `detect_esp_ports`, `probe_port` | Hotplug discovery |
| `models` | JSON request/response types and CLI command mappers |
| `csi` | COBS/postcard frame decoder |
| `routes` | Axum handler functions (for custom router extension) |
| `serial`, `parquet_sink` | Lower-level pipelines |

## Custom router

Mount the default routes or compose your own:

```rust
use axum::Router;
use csi_webserver_core::{build_router, AppState};

let state = AppState::new();
let app: Router = build_router(state);
// or nest `build_router(state)` under your own paths
```

## Migration from pre-0.1.5 single crate

| Before | After |
|--------|-------|
| `csi_webserver::run_supervisor` | `csi_webserver_core::run_supervisor` |
| `spawn_device` + `registry.insert` | `registry.attach(DeviceAttachSpec { ... })` |
| Binary in same crate | Use `csi-webserver` or embed this library |

## Related crates

| Crate | Role |
|-------|------|
| `csi-webserver-core` | This library |
| `csi-webserver` | Default executable + [HTTP API](https://github.com/csi-rs/csi-webserver-rs/blob/main/API.md) |

## License

Apache-2.0. See [LICENSE](LICENSE).
