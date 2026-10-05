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
- Parquet session dumps decoded from the firmware's serialized frames
- USB hotplug supervisor ([`supervisor`](src/supervisor.rs))
- Explicit device registration ([`DeviceRegistry::attach`](src/state.rs))

## Node modes

`POST /api/devices/{id}/config/wifi` mirrors the firmware's `set-wifi` grammar. Its `mode` names
one **operational mode** — how a node reaches the channel. Of the other three attributes that
describe a node, the network role follows from the mode, the reporting policy is the optional
`collection` field (below), and the host is the session's controller.

The model is documented once, in
[`esp-csi-rs/docs/network-model.md`](https://github.com/csi-rs/esp-csi-rs/blob/main/docs/network-model.md).
This crate validates against it and does not restate it.

| Mode | Reaches the channel by |
|---|---|
| `station` | associating to an AP or a commercial router |
| `sniffer` | promiscuous capture on a locked channel |
| `wifi-ap` | a self-contained softAP with DHCP |
| `ht20-emitter` / `ht40-emitter` | unassociated raw 802.11n injection, 20 or 40 MHz |
| `esp-now-central` / `esp-now-peripheral` | the symmetric connectionless exchange |
| `esp-now-fast-source` / `esp-now-fast-collector` | the asymmetric exchange; also spelled `esp-now-simplex-source` / `esp-now-simplex-peer` |

`peer_mac` is the emitter's injection destination and the explicit ESP-NOW peer (set it on both
nodes); `ht40` is the softAP secondary channel in `wifi-ap` and the forced per-peer TX PHY in the
ESP-NOW modes. `channel` is forwarded as given: 1–14 on 2.4 GHz, plus the 5 GHz channels on the
ESP32-C5 (default 149).

### Collection mode

`"collection": "collector" | "listener"` (optional) is sent as `set-wifi --collection=`. A collector
reports the CSI it captures; a listener captures but does not report.

| Mode | `collection` |
|---|---|
| `station`, `wifi-ap`, `esp-now-central`, `esp-now-peripheral` | accepted, either value |
| `sniffer`, `esp-now-fast-collector` / `esp-now-simplex-peer` | **400** — fixed collector |
| `ht20-emitter`, `ht40-emitter`, `esp-now-fast-source` / `esp-now-simplex-source` | **400** — fixed listener |
| a mode added by a `CsiProfile` | passed through unchecked |

Omit the field to leave the device's stored value alone (the firmware default is `collector`). The
check is made against the `mode` in the same request, which this route requires.

Modes this crate does not name — chip-gated ones, or those supplied by a build it does not target —
are added by an embedder through
[`CsiProfile::extra_wifi_modes`](src/profile.rs); their mode-specific flags ride through the
flattened `extra` map on the request body and re-emit verbatim as `--{key}={value}`, so this crate
never names any of them.

### Delivery gate

`POST /api/devices/{id}/config/csi-output` (`{ "enabled": true|false }`) is the runtime delivery
gate: it switches off-device delivery of captured CSI, and capture and its RX timing are unchanged
either way. It is separate from `collection` above.

`POST /api/devices/{id}/config/rate` is reporting only, except on the ESP-NOW pair
(`esp-now-central` / `esp-now-peripheral`), which applies it.

## Decoding

[`csi::decode_frame`](src/csi.rs) reads esp-csi-rs's versioned wire format (the module is vendored
in [`src/wire`](src/wire), unmodified) and falls back to the pre-0.12 per-chip layouts, so mixed
firmware decodes. Session announcements are logged; measurements become a `DecodedCsi`.

Parquet files carry `csi_schema_version` in their metadata. Version 2 adds node and session ids,
the frame counter, a 64-bit `timestamp_us`, PPDU format, bandwidth, the subcarrier layout with
`subcarrier_index` / `subcarrier_freq_hz`, stimulus (setup and instance ids), the MAC-header digest,
and the variation and grouped report payloads. They are null for frames from older firmware.

## Quick embed

```toml
[dependencies]
csi-webserver-core = "0.3"
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
| `csi` | Serialized frame decoder |
| `wire` | esp-csi-rs's wire format (vendored) |
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

## Related crates

| Crate | Role |
|-------|------|
| `csi-webserver-core` | This library |
| `csi-webserver` | Default executable + [HTTP API](https://github.com/csi-rs/csi-webserver-rs/blob/main/API.md) |

## License

Apache-2.0. See [LICENSE](LICENSE).
