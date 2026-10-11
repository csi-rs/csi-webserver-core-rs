//! Data models used by HTTP handlers and runtime control flow.
//!
//! This module contains:
//! - request-body structs for config/control endpoints,
//! - runtime enums used by watch channels,
//! - common API response payloads.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::profile::CsiProfile;

// ─── Device config (cached state) ─────────────────────────────────────────

/// Server-side cached view of device-side `UserConfig`, structured to mirror
/// the firmware's `show-config` output (sections `[WiFi]`, `[Collection]`,
/// `[CSI Config]`). Fields are best-effort: each is populated when the
/// matching `POST /api/devices/{id}/config/*` endpoint succeeds, and reset to
/// firmware defaults by `POST /api/devices/{id}/config/reset`. Values can drift if the device is
/// re-flashed or commands are sent out-of-band.
///
/// `sta_password` is intentionally *not* cached even though `show-config`
/// echoes it — round-tripping plaintext passwords through a GET endpoint
/// would defeat the point of having one.
///
/// The trailing fields (`csi_delivery_mode`, `csi_logging_enabled`) live
/// alongside the show-config sections because they're set via separate CLI
/// commands (`set-csi-delivery`) and are useful to surface here even though
/// they aren't part of the `show-config` block.
///
/// The log mode is fixed to `serialized` (the only format this server
/// consumes), so it is no longer a configurable field.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceConfig {
    pub wifi: WifiSection,
    pub collection: CollectionSection,
    pub csi_config: CsiConfigSection,
    pub csi_delivery_mode: Option<String>,
    pub csi_logging_enabled: Option<bool>,
}

/// `[WiFi]` section in `show-config`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WifiSection {
    /// `node_mode` — the **operational mode**: one of the `set-wifi --mode=`
    /// values in [`WIFI_MODES`] (Wi-Fi station, sniffer and access point, the
    /// two emitters, and the ESP-NOW and ESP-NOW simplex ends), plus any mode
    /// the active [`CsiProfile`] adds.
    pub mode: Option<String>,
    /// `channel` — `u8`, the primary channel number. 2.4 GHz channels are
    /// 1..=14; the dual-band ESP32-C5 also takes 5 GHz channels (its default is
    /// 149). This crate forwards the number unvalidated — which channels the
    /// radio can actually use depends on the chip.
    pub channel: Option<u8>,
    /// `sta_ssid` — UTF-8, ≤ 32 B.
    pub sta_ssid: Option<String>,
    /// `ap_ssid` — softAP SSID for `wifi-ap` mode. Default `esp-csi-ap`.
    pub ap_ssid: Option<String>,
    /// `ap_password` — intentionally not cached (same policy as `sta_password`).
    pub ap_password: Option<String>,
    /// `serve_dhcp` — built-in DHCP server in `wifi-ap` mode.
    pub ap_dhcp: Option<bool>,
    /// `ap_lease_count` — DHCP lease pool size in `wifi-ap` mode (1–8).
    /// Reported as `AP Leases` in `show-config`.
    pub ap_leases: Option<u8>,
    /// `ap_sync_burst` — synchronized burst flood in `wifi-ap` mode.
    /// Reported as `AP Burst` in `show-config`.
    pub ap_burst: Option<bool>,
    /// `peer_mac` — reported as `Dst MAC` in `show-config`; `broadcast` is
    /// the firmware default. The destination of the frames an emitter
    /// injects, and the explicit peer in the ESP-NOW modes (set it on both
    /// nodes). Ignored by the other modes.
    pub peer_mac: Option<String>,
    /// `ht40_secondary` — `above` | `below` | `none` (`none` = HT20/legacy).
    /// The softAP secondary channel in `wifi-ap` mode, and the forced
    /// per-peer TX PHY in the ESP-NOW modes.
    pub ht40: Option<String>,
    /// The node's stored **reporting** setting, set with `set-wifi
    /// --reporting=`. Only the modes that admit a choice read it (see
    /// [`WifiConfig::reporting`]); the firmware default is `report`.
    pub reporting: Option<Reporting>,
}

/// `[Collection]` section in `show-config`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CollectionSection {
    /// `csi_output_enabled` — reported as `CSI Output` in `show-config`.
    /// Whether captured CSI is delivered off-device; capture itself keeps
    /// running either way. Set via [`CsiOutputConfig`].
    pub csi_output_enabled: Option<bool>,
    /// Measurement periodicity in Hz (`set-measurement --periodicity-hz=`),
    /// reported as `Periodicity` in `show-config`. `0` disables generated traffic.
    pub periodicity_hz: Option<u64>,
    /// `flood_unsolicited` — ICMP flood sends unsolicited echo replies
    /// (one-directional traffic) instead of echo requests.
    pub unsolicited: Option<bool>,
    /// `phy_rate` enum — e.g. `mcs0-lgi`. Reporting only, except on the
    /// ESP-NOW pair (`esp-now-initiator` / `esp-now-responder`), which applies it.
    pub phy_rate: Option<String>,
    /// `protocol` — Wi-Fi PHY protocol applied at the start of each run.
    /// One of `b` | `g` | `n` | `lr` | `a` | `ac`; defaults to `lr`.
    pub protocol: Option<String>,
    /// `io_tasks.tx_enabled`.
    pub io_tx_enabled: Option<bool>,
    /// `io_tasks.rx_enabled`.
    pub io_rx_enabled: Option<bool>,
}

/// `[CSI Config]` section in `show-config`. The classic (ESP32 / C3 / S3) and
/// extended (ESP32-C5 / C6) fields are merged into a single struct so the JSON
/// shape is stable across chip variants. The fields applicable to the active
/// firmware are populated; the others remain `None`.
///
/// The classic block also includes three read-only fields
/// (`channel_filter_enabled`, `manual_scale`, `shift`) that have no `set-csi`
/// flag; they are populated by `POST /api/devices/{id}/config/reset` from firmware defaults
/// but otherwise stay fixed.
///
/// Any acquisition flag the core does not name (supplied by an embedder's
/// [`CsiProfile`] or a profile-aware client) is
/// carried verbatim in [`extra`](Self::extra) and re-emitted generically.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CsiConfigSection {
    // ── Classic (ESP32 / C3 / S3) ─────────────────────────────────────
    /// `lltf_en`.
    pub lltf_enabled: Option<bool>,
    /// `htltf_en`.
    pub htltf_enabled: Option<bool>,
    /// `stbc_htltf2_en`.
    pub stbc_htltf_enabled: Option<bool>,
    /// `ltf_merge_en`.
    pub ltf_merge_enabled: Option<bool>,
    /// `channel_filter_en` — **read-only**; only restored by `reset-config`.
    pub channel_filter_enabled: Option<bool>,
    /// `manu_scale` — **read-only**; only restored by `reset-config`.
    pub manual_scale: Option<bool>,
    /// `shift` — **read-only**; only restored by `reset-config`.
    pub shift: Option<u8>,
    /// `dump_ack_en` — configurable on C5/C6 chips via `set-csi --dump-ack=`.
    pub dump_ack_enabled: Option<bool>,

    // ── Extended (ESP32-C5 / C6) ──────────────────────────────────────
    /// `enable` (acquire CSI overall).
    pub acquire_csi: Option<u32>,
    /// `acquire_csi_legacy` — L-LTF / 11g.
    pub acquire_csi_legacy: Option<u32>,
    /// `acquire_csi_ht20`.
    pub acquire_csi_ht20: Option<u32>,
    /// `acquire_csi_ht40`.
    pub acquire_csi_ht40: Option<u32>,
    /// `val_scale_cfg`.
    pub val_scale_cfg: Option<u32>,
    /// Force L-LTF acquisition (ESP32-C5 only).
    pub acquire_csi_force_lltf: Option<bool>,
    /// VHT-LTF for VHT20 PPDUs (ESP32-C5 only).
    pub acquire_csi_vht: Option<bool>,

    /// Acquisition flags the core does not name. Populated from
    /// [`CsiConfig::apply_to_cache`] passthrough or a profile's
    /// [`resolve_preset`](crate::profile::CsiProfile::resolve_preset); each
    /// entry re-emits as `--{key}={value}` in the `set-csi` command.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl DeviceConfig {
    /// Snapshot of `UserConfig::new()` / `CsiConfig::default()` on the
    /// device, as documented in the `show-config` spec. Populated into
    /// the cache by `POST /api/devices/{id}/config/reset` so the response after a
    /// reset reflects what the firmware actually holds, even before the
    /// user re-sends any `set-*` commands.
    ///
    /// Both the classic and the extended (C5/C6) CSI defaults are populated —
    /// the caller can ignore the fields irrelevant to the connected chip
    /// (consult `GET /api/devices/{id}/info` for the `chip` field).
    pub fn firmware_defaults() -> Self {
        Self::firmware_defaults_for_chip(None)
    }

    /// Like [`firmware_defaults`](Self::firmware_defaults), with chip-specific Wi-Fi channel (C5 → 149,
    /// C6 → 6, others → 1) matching the esp-csi-cli-rs lab examples.
    pub fn firmware_defaults_for_chip(chip: Option<&str>) -> Self {
        Self {
            wifi: WifiSection {
                mode: Some("sniffer".to_string()),
                channel: Some(default_wifi_channel(chip)),
                sta_ssid: Some(String::new()),
                ap_ssid: Some("esp-csi-ap".to_string()),
                ap_password: None,
                ap_dhcp: Some(true),
                ap_leases: Some(4),
                ap_burst: Some(false),
                peer_mac: Some("broadcast".to_string()),
                ht40: Some("none".to_string()),
                reporting: Some(Reporting::Report),
            },
            collection: CollectionSection {
                csi_output_enabled: Some(true),
                periodicity_hz: Some(100),
                unsolicited: Some(false),
                phy_rate: Some("mcs0-lgi".to_string()),
                protocol: Some("lr".to_string()),
                io_tx_enabled: Some(true),
                io_rx_enabled: Some(true),
            },
            csi_config: CsiConfigSection {
                // Classic
                lltf_enabled: Some(true),
                htltf_enabled: Some(true),
                stbc_htltf_enabled: Some(true),
                ltf_merge_enabled: Some(true),
                channel_filter_enabled: Some(false),
                manual_scale: Some(false),
                shift: Some(0),
                dump_ack_enabled: Some(true),
                // Extended (C5/C6 defaults — classic chips ignore these fields)
                acquire_csi: Some(1),
                acquire_csi_legacy: Some(1),
                acquire_csi_ht20: Some(1),
                acquire_csi_ht40: Some(1),
                val_scale_cfg: Some(2),
                acquire_csi_force_lltf: Some(true),
                acquire_csi_vht: Some(true),
                extra: BTreeMap::new(),
            },
            csi_delivery_mode: None,
            csi_logging_enabled: None,
        }
    }
}

/// Default Wi-Fi channel for a chip, matching the esp-csi-cli-rs lab examples.
pub fn default_wifi_channel(chip: Option<&str>) -> u8 {
    match chip.map(|c| c.trim().to_ascii_lowercase()) {
        Some(ref c) if c == "esp32c5" => 149,
        Some(ref c) if c == "esp32c6" => 6,
        _ => 1,
    }
}

// ─── Quoting helpers ──────────────────────────────────────────────────────

/// Quote a free-form string argument for `esp-csi-cli-rs`.
///
/// The CLI accepts both `'…'` and `"…"`; the opening quote style is
/// matched by the same style and the other quote is treated literally.
/// Spaces inside quotes are forwarded as `0x1F` and decoded back to `' '`
/// in the device-side handler. Underscores are passed through literally
/// (no shorthand substitution).
fn quote_cli_arg(s: &str) -> Result<String, String> {
    if s.contains('\n') || s.contains('\r') {
        return Err("value cannot contain newline characters".to_string());
    }
    if !s.contains('\'') {
        Ok(format!("'{s}'"))
    } else if !s.contains('"') {
        Ok(format!("\"{s}\""))
    } else {
        Err("value cannot contain both single and double quote characters".to_string())
    }
}

// ─── HTTP request bodies ───────────────────────────────────────────────────

/// Validate a `peer_mac` (emitter destination / ESP-NOW peer) the way the firmware does:
/// six hex octets separated by `:` or `-` (case-insensitive). An empty string is
/// *valid* and means "clear back to broadcast".
fn validate_peer_mac(mac: &str) -> Result<(), String> {
    if mac.is_empty() {
        return Ok(());
    }
    let sep = if mac.contains(':') {
        ':'
    } else if mac.contains('-') {
        '-'
    } else {
        return Err("peer_mac must use ':' or '-' separators (aa:bb:cc:dd:ee:ff)".to_string());
    };
    let octets: Vec<&str> = mac.split(sep).collect();
    if octets.len() != 6 || octets.iter().any(|o| o.len() != 2 || !o.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(format!("Invalid peer_mac '{mac}' (use aa:bb:cc:dd:ee:ff)"));
    }
    Ok(())
}

/// Accepted `set-wifi --mode=` values — one per **mode** the firmware offers.
///
/// A node is described by its mode, session role (initiator or responder; none for an observer),
/// sensing role (transmitter, receiver or both) and reporting. The session and sensing roles follow
/// from the mode here; reporting is chosen with [`WifiConfig::reporting`] in the modes that admit
/// a choice ([`FIXED_REPORTING_MODES`] lists the ones that do not). The host only starts and stops
/// runs; it is never a session initiator. The model is documented once, in
/// [`docs/network-model.md`](https://github.com/csi-rs/esp-csi-rs/blob/main/docs/network-model.md),
/// and deliberately not restated here.
///
/// This list must stay a superset of what the firmware accepts.
///
/// Modes this crate does not name — chip-gated ones, or those supplied by a build it does not
/// target — arrive through
/// [`CsiProfile::extra_wifi_modes`] with their
/// mode-specific flags riding in [`WifiConfig::extra`].
pub const WIFI_MODES: &[&str] = &[
    "station",
    "sniffer",
    "wifi-ap",
    "ht20-emitter",
    "ht40-emitter",
    "esp-now-initiator",
    "esp-now-responder",
    "esp-now-simplex-source",
    "esp-now-simplex-peer",
];

/// Base modes whose reporting is fixed by the mode itself, paired with that fixed value.
/// Sending [`WifiConfig::reporting`] with one of these is rejected rather than silently ignored.
///
/// The sniffer must report to observe anything; the emitters measure nothing, so they report
/// nothing; a simplex source only transmits (reports nothing) and a simplex peer only receives
/// (reports). Every other base mode — station, access point and the symmetric ESP-NOW pair —
/// admits either value, and a mode supplied by a [`CsiProfile`] is passed through unchecked.
pub const FIXED_REPORTING_MODES: &[(&str, Reporting)] = &[
    ("sniffer", Reporting::Report),
    ("ht20-emitter", Reporting::None),
    ("ht40-emitter", Reporting::None),
    ("esp-now-simplex-source", Reporting::None),
    ("esp-now-simplex-peer", Reporting::Report),
];

/// A node's **reporting** — whether the measurements it takes are reported to the host.
/// Serialized lowercase (`"report"` / `"none"`) and sent as `set-wifi --reporting=`.
///
/// A node that reports delivers the CSI it measures; one set to `none` measures but reports
/// nothing. This is distinct from [`CsiOutputConfig`], which is only a runtime gate on delivery.
/// The firmware's threshold-based and decimated reporting, and `--report-to=`, are not modelled
/// here; send them through [`WifiConfig::extra`] if needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reporting {
    /// Measures CSI and reports it — its own, and any reported to it by peers.
    Report,
    /// Measures CSI but reports nothing.
    None,
}

impl Reporting {
    /// The CLI token: `report` or `none`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::None => "none",
        }
    }
}

/// `POST /api/devices/{id}/config/wifi` — the `set-wifi` command.
#[derive(Debug, Deserialize)]
pub struct WifiConfig {
    /// One of the values in [`WIFI_MODES`].
    pub mode: String,
    pub sta_ssid: Option<String>,
    pub sta_password: Option<String>,
    /// SoftAP SSID for `wifi-ap` mode.
    pub ap_ssid: Option<String>,
    /// SoftAP password for `wifi-ap` mode; empty = open network.
    pub ap_password: Option<String>,
    /// Enable built-in DHCP in `wifi-ap` mode.
    pub ap_dhcp: Option<bool>,
    /// DHCP lease pool size in `wifi-ap` mode (1–8). With more than one
    /// lease the ICMP flood targets every associated station.
    pub ap_leases: Option<u8>,
    /// Synchronized burst flood in `wifi-ap` mode: every flood tick sends one
    /// unicast frame back-to-back to every active lease (time-aligned
    /// multi-receiver CSI) instead of round-robining one station per tick.
    pub ap_burst: Option<bool>,
    pub channel: Option<u8>,
    /// Peer address (`aa:bb:cc:dd:ee:ff` or `aa-bb-...`). In the emitter
    /// modes it is the destination of the injected frames — unicasting to a
    /// receiving node's MAC usually raises that node's CSI rate. In the
    /// ESP-NOW modes it is the explicit peer, replacing discovery; set it on
    /// both nodes. An empty string clears it back to broadcast. The other
    /// modes ignore it.
    pub peer_mac: Option<String>,
    /// `above` | `below` | `none` | `off` (`off` is a firmware-side alias for
    /// `none`). The softAP secondary channel in `wifi-ap` mode, and the forced
    /// per-peer TX PHY in the ESP-NOW modes.
    pub ht40: Option<String>,
    /// Which interface an emitter injects on: `sta` | `ap`. The driver
    /// accepts raw TX on both, but which one actually radiates is
    /// chip-dependent, so this is the first thing to flip when a receiver
    /// sees nothing while the emitter reports no rejected frames. Emitter
    /// modes only; the firmware ignores it elsewhere and defaults to the
    /// STA interface when the flag is omitted, so `None` sends nothing.
    pub emitter_iface: Option<String>,
    /// The node's reporting, sent as `--reporting=report|none`.
    /// Accepted for `station`, `wifi-ap`, `esp-now-initiator` and
    /// `esp-now-responder`, and passed through for profile-supplied modes.
    /// Rejected for the modes in [`FIXED_REPORTING_MODES`], whose reporting
    /// is fixed. Omitted sends nothing, so the firmware keeps its stored
    /// value (default `report`).
    pub reporting: Option<Reporting>,
    /// Parameters the core does not name are carried verbatim here and
    /// re-emitted generically as `--{key}={value}` (same convention as
    /// [`CsiConfig::extra`]). This lets an embedder's [`CsiProfile`] accept
    /// modes (see [`CsiProfile::extra_wifi_modes`]) that need flags the open
    /// core does not know — without the core naming any of them.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl WifiConfig {
    /// Validate values and emit the matching `set-wifi …` line.
    ///
    /// `chip` is used to pick the firmware default channel when `channel` is
    /// omitted for non-`station` modes (C5 → 149, C6 → 6). `profile` supplies
    /// any extra `--mode` values the open core does not name.
    pub fn to_cli_command(
        &self,
        chip: Option<&str>,
        profile: &dyn CsiProfile,
    ) -> Result<String, String> {
        let extra_modes = profile.extra_wifi_modes();
        if !WIFI_MODES.contains(&self.mode.as_str()) && !extra_modes.contains(&self.mode.as_str()) {
            let mut accepted: Vec<&str> = WIFI_MODES.to_vec();
            accepted.extend_from_slice(extra_modes);
            return Err(format!(
                "Unknown wifi mode '{}'; expected one of: {}",
                self.mode,
                accepted.join(", ")
            ));
        }

        let mut cmd = format!("set-wifi --mode={}", self.mode);

        if let Some(ssid) = &self.sta_ssid {
            if ssid.len() > 32 {
                return Err(format!(
                    "sta_ssid is {} bytes; firmware limit is 32 bytes",
                    ssid.len()
                ));
            }
            cmd.push_str(&format!(" --sta-ssid={}", quote_cli_arg(ssid)?));
        }

        if let Some(pass) = &self.sta_password {
            if pass.len() > 32 {
                return Err(format!(
                    "sta_password is {} bytes; firmware limit is 32 bytes",
                    pass.len()
                ));
            }
            cmd.push_str(&format!(" --sta-password={}", quote_cli_arg(pass)?));
        }

        if let Some(ssid) = &self.ap_ssid {
            if ssid.len() > 32 {
                return Err(format!(
                    "ap_ssid is {} bytes; firmware limit is 32 bytes",
                    ssid.len()
                ));
            }
            cmd.push_str(&format!(" --ap-ssid={}", quote_cli_arg(ssid)?));
        }

        if let Some(pass) = &self.ap_password {
            if pass.len() > 32 {
                return Err(format!(
                    "ap_password is {} bytes; firmware limit is 32 bytes",
                    pass.len()
                ));
            }
            cmd.push_str(&format!(" --ap-password={}", quote_cli_arg(pass)?));
        }

        if let Some(dhcp) = self.ap_dhcp {
            cmd.push_str(if dhcp {
                " --ap-dhcp=on"
            } else {
                " --ap-dhcp=off"
            });
        }

        if let Some(leases) = self.ap_leases {
            if !(1..=8).contains(&leases) {
                return Err(format!("ap_leases is {leases}; firmware accepts 1-8"));
            }
            cmd.push_str(&format!(" --ap-leases={leases}"));
        }

        if let Some(burst) = self.ap_burst {
            cmd.push_str(if burst {
                " --ap-burst=on"
            } else {
                " --ap-burst=off"
            });
        }

        // Channel handling by mode:
        // - Non-station modes: send the operating channel, defaulting to the
        //   chip's channel when the client omits it.
        // - Station mode: the channel is an optional pre-association
        //   band-selection hint (`WifiStationConfig::channel_hint`, meaningful
        //   on the ESP32-C5's 5 GHz band). Forward it only when explicitly
        //   provided; when omitted the firmware derives the channel from the
        //   associated AP, so we send nothing.
        if let Some(ch) = self
            .channel
            .or_else(|| (self.mode != "station").then(|| default_wifi_channel(chip)))
        {
            cmd.push_str(&format!(" --set-channel={ch}"));
        }

        if let Some(mac) = &self.peer_mac {
            validate_peer_mac(mac)?;
            // An empty value is forwarded verbatim (`--peer-mac=`) so the
            // firmware clears the destination back to broadcast.
            cmd.push_str(&format!(" --peer-mac={mac}"));
        }

        if let Some(ht40) = &self.ht40 {
            match ht40.as_str() {
                "above" | "below" | "none" | "off" => {}
                other => {
                    return Err(format!("Invalid ht40 '{other}' (use above, below, none, or off)"));
                }
            }
            cmd.push_str(&format!(" --ht40={ht40}"));
        }

        if let Some(iface) = &self.emitter_iface {
            match iface.as_str() {
                "sta" | "ap" => {}
                other => {
                    return Err(format!("Invalid emitter_iface '{other}' (use sta or ap)"));
                }
            }
            cmd.push_str(&format!(" --emitter-iface={iface}"));
        }

        if let Some(reporting) = self.reporting {
            if let Some((_, fixed)) = FIXED_REPORTING_MODES
                .iter()
                .find(|(mode, _)| *mode == self.mode)
            {
                return Err(format!(
                    "mode '{}' has a fixed reporting ({}); omit reporting",
                    self.mode,
                    fixed.as_str()
                ));
            }
            cmd.push_str(&format!(" --reporting={}", reporting.as_str()));
        }

        // Profile-supplied / unknown params (e.g. an emitter's inter-frame
        // period) ride in `extra` and re-emit generically as `--{key}={value}`.
        for (key, value) in &self.extra {
            push_extra(&mut cmd, key, value);
        }

        Ok(cmd)
    }
}

/// `POST /api/devices/{id}/config/measurement` — the `set-measurement` command.
#[derive(Debug, Deserialize)]
pub struct MeasurementConfig {
    /// Measurement periodicity in Hz — the rate of the traffic the node generates to
    /// measure on; `0` disables generation.
    pub periodicity_hz: u64,
    /// `--unsolicited=on|off` (default off). When on, the ICMP flood sends
    /// unsolicited echo *replies* instead of echo requests: the peer silently
    /// ignores them at the IP level, so traffic is strictly one-directional —
    /// stable offered rate, but the flooding node captures no CSI back from
    /// replies. Omitted → flag not forwarded, firmware keeps its default.
    pub unsolicited: Option<bool>,
}

impl MeasurementConfig {
    pub fn to_cli_command(&self) -> String {
        let mut cmd = format!("set-measurement --periodicity-hz={}", self.periodicity_hz);
        push_on_off(&mut cmd, "unsolicited", self.unsolicited);
        cmd
    }
}

/// CSI feature flags. Classic (ESP32 / ESP32-C3 / ESP32-S3) and extended
/// (ESP32-C5 / ESP32-C6) parameters are merged here; the firmware will
/// silently ignore flags that are not part of its compiled-in variant.
///
/// Flags the core does not name are accepted via the flattened [`extra`](Self::extra)
/// map and re-emitted generically as `--{key}={value}`, so an embedder's
/// [`CsiProfile`] or a profile-aware client can drive
/// acquisition options this build has no dedicated field for.
#[derive(Debug, Deserialize)]
pub struct CsiConfig {
    // ── Classic (non-C5/C6) ────────────────────────────────────────────
    /// `--lltf=on|off`
    pub lltf: Option<bool>,
    /// `--htltf=on|off`
    pub htltf: Option<bool>,
    /// `--stbc-htltf=on|off`
    pub stbc_htltf: Option<bool>,
    /// `--ltf-merge=on|off`
    pub ltf_merge: Option<bool>,
    // ── Extended (C5/C6) ───────────────────────────────────────────────
    /// `--csi=on|off` — master acquisition switch.
    pub csi: Option<bool>,
    /// `--csi-legacy=on|off`
    pub csi_legacy: Option<bool>,
    /// `--csi-ht20=on|off`
    pub csi_ht20: Option<bool>,
    /// `--csi-ht40=on|off`
    pub csi_ht40: Option<bool>,
    /// `--dump-ack=on|off` (C5/C6 chips).
    pub dump_ack: Option<bool>,
    /// `--csi-force-lltf=on|off` (ESP32-C5 only).
    pub csi_force_lltf: Option<bool>,
    /// `--csi-vht=on|off` (ESP32-C5 only).
    pub csi_vht: Option<bool>,
    /// `0..=3`; default `2`.
    pub val_scale_cfg: Option<u32>,
    /// Flags the core does not name (including a `preset` key resolved via the
    /// active [`CsiProfile`]). Each entry other
    /// than `preset` re-emits as `--{key}={value}`.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl CsiConfig {
    pub fn to_cli_command(&self, profile: &dyn CsiProfile) -> Result<String, String> {
        let mut cmd = "set-csi".to_string();

        // A `preset` arrives via the flattened `extra` map. The core knows only
        // `default`; anything else is resolved through the active profile.
        if let Some(preset) = self.extra.get("preset") {
            let name = preset
                .as_str()
                .ok_or_else(|| "preset must be a string".to_string())?;
            if name == "default" {
                cmd.push_str(" --preset=default");
                return Ok(cmd);
            }
            if let Some(cli) = profile.preset_cli(name) {
                return Ok(cli);
            }
            return Err(format!("unknown preset '{name}'; expected default"));
        }

        push_on_off(&mut cmd, "lltf", self.lltf);
        push_on_off(&mut cmd, "htltf", self.htltf);
        push_on_off(&mut cmd, "stbc-htltf", self.stbc_htltf);
        push_on_off(&mut cmd, "ltf-merge", self.ltf_merge);
        push_on_off(&mut cmd, "csi", self.csi);
        push_on_off(&mut cmd, "csi-legacy", self.csi_legacy);
        push_on_off(&mut cmd, "csi-ht20", self.csi_ht20);
        push_on_off(&mut cmd, "csi-ht40", self.csi_ht40);
        push_on_off(&mut cmd, "dump-ack", self.dump_ack);
        push_on_off(&mut cmd, "csi-force-lltf", self.csi_force_lltf);
        push_on_off(&mut cmd, "csi-vht", self.csi_vht);

        if let Some(scale) = self.val_scale_cfg {
            cmd.push_str(&format!(" --val-scale-cfg={scale}"));
        }

        // Generic passthrough for any flag the core does not name.
        for (key, value) in &self.extra {
            push_extra(&mut cmd, key, value);
        }
        Ok(cmd)
    }

    /// Apply this request body to the server-side config cache.
    pub fn apply_to_cache(&self, cfg: &mut CsiConfigSection, profile: &dyn CsiProfile) {
        if let Some(preset) = self.extra.get("preset") {
            if let Some(name) = preset.as_str() {
                *cfg = profile
                    .resolve_preset(name)
                    .unwrap_or_else(|| DeviceConfig::firmware_defaults().csi_config);
                return;
            }
        }
        apply_bool_cache(&mut cfg.lltf_enabled, self.lltf);
        apply_bool_cache(&mut cfg.htltf_enabled, self.htltf);
        apply_bool_cache(&mut cfg.stbc_htltf_enabled, self.stbc_htltf);
        apply_bool_cache(&mut cfg.ltf_merge_enabled, self.ltf_merge);
        apply_u32_cache(&mut cfg.acquire_csi, self.csi);
        apply_u32_cache(&mut cfg.acquire_csi_legacy, self.csi_legacy);
        apply_u32_cache(&mut cfg.acquire_csi_ht20, self.csi_ht20);
        apply_u32_cache(&mut cfg.acquire_csi_ht40, self.csi_ht40);
        apply_bool_cache(&mut cfg.dump_ack_enabled, self.dump_ack);
        apply_bool_cache(&mut cfg.acquire_csi_force_lltf, self.csi_force_lltf);
        apply_bool_cache(&mut cfg.acquire_csi_vht, self.csi_vht);
        if let Some(scale) = self.val_scale_cfg {
            cfg.val_scale_cfg = Some(scale);
        }
        // Carry unknown flags into the cache verbatim so `GET /api/devices/{id}/config`
        // round-trips whatever a profile-aware client set.
        for (key, value) in &self.extra {
            if key == "preset" {
                continue;
            }
            cfg.extra.insert(key.clone(), value.clone());
        }
    }
}

fn push_on_off(cmd: &mut String, flag: &str, value: Option<bool>) {
    if let Some(v) = value {
        cmd.push_str(&format!(" --{flag}={}", if v { "on" } else { "off" }));
    }
}

/// Render one unnamed acquisition flag as `--{key}={value}`, reusing the same
/// on/off + numeric conventions the named flags use.
fn push_extra(cmd: &mut String, key: &str, value: &serde_json::Value) {
    use serde_json::Value;
    let rendered = match value {
        Value::Bool(b) => (if *b { "on" } else { "off" }).to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    cmd.push_str(&format!(" --{key}={rendered}"));
}

fn apply_bool_cache(slot: &mut Option<bool>, value: Option<bool>) {
    if let Some(v) = value {
        *slot = Some(v);
    }
}

fn apply_u32_cache(slot: &mut Option<u32>, value: Option<bool>) {
    if let Some(v) = value {
        *slot = Some(u32::from(v));
    }
}

/// `POST /api/devices/{id}/config/csi-output` — gate off-device delivery of captured CSI.
///
/// This is the runtime delivery gate only. The node's reporting is a
/// separate setting, [`WifiConfig::reporting`] (`set-wifi --reporting=`). Capture keeps
/// running with delivery off, so the RX path and its timing are unchanged —
/// nothing is decoded or logged. An emitter captures nothing, so the gate has
/// no effect there.
#[derive(Debug, Deserialize)]
pub struct CsiOutputConfig {
    /// `--enabled=true|false`; the firmware default is `true`.
    pub enabled: bool,
}

impl CsiOutputConfig {
    /// Unlike the mode string this replaced, a bool has no invalid value, so
    /// the command cannot fail to build.
    pub fn to_cli_command(&self) -> String {
        format!("set-csi-output --enabled={}", self.enabled)
    }
}

#[derive(Debug, Deserialize)]
pub struct StartConfig {
    /// Collection duration in seconds; omit for indefinite collection.
    pub duration: Option<u64>,
}

impl StartConfig {
    pub fn to_cli_command(&self) -> String {
        match self.duration {
            Some(d) => format!("start --duration={d}"),
            None => "start".to_string(),
        }
    }
}

/// `POST /api/devices/{id}/config/rate` — set the Wi-Fi PHY rate. Reporting
/// only (it is echoed in `show-config`), except on the ESP-NOW pair
/// (`esp-now-initiator` / `esp-now-responder`), which applies it.
#[derive(Debug, Deserialize)]
pub struct RateConfig {
    /// e.g. `1m`, `2m`, `5m5`, `11m`, `6m`..`54m`, `mcs0-lgi`..`mcs7-lgi`,
    /// `mcs0-sgi`.
    pub rate: String,
}

impl RateConfig {
    pub fn to_cli_command(&self) -> String {
        format!("set-rate --rate={}", self.rate)
    }
}

/// `POST /api/devices/{id}/config/protocol` — set the Wi-Fi PHY protocol applied to the node
/// at the start of each collection run.
#[derive(Debug, Deserialize)]
pub struct ProtocolConfig {
    /// One of `b` | `g` | `n` | `lr` | `a` | `ac`, plus any value the active
    /// [`CsiProfile`] advertises.
    pub protocol: String,
}

impl ProtocolConfig {
    /// Accepted protocol values, matching the firmware's `set-protocol` parser.
    pub const VALID: &[&str] = &["b", "g", "n", "lr", "a", "ac"];

    /// Build the `set-protocol` command, rejecting unknown values up front.
    /// Accepts [`Self::VALID`] plus the profile's
    /// [`extra_protocols`](crate::profile::CsiProfile::extra_protocols). This is
    /// string-level validation only — the radio may still reject a protocol the
    /// specific chip/band doesn't support, which surfaces at `start`, not here.
    pub fn to_cli_command(&self, profile: &dyn CsiProfile) -> Result<String, String> {
        let protocol = self.protocol.to_ascii_lowercase();
        let accepted = profile.extra_protocols();
        if !Self::VALID.contains(&protocol.as_str()) && !accepted.contains(&protocol.as_str()) {
            let mut valid: Vec<&str> = Self::VALID.to_vec();
            valid.extend_from_slice(accepted);
            return Err(format!(
                "unknown protocol '{}'; expected one of: {}",
                self.protocol,
                valid.join(", "),
            ));
        }
        Ok(format!("set-protocol --protocol={protocol}"))
    }
}

/// `POST /api/devices/{id}/config/io-tasks` — toggle the per-direction TX/RX Embassy tasks.
/// Both fields are independently optional; omitted fields keep their current
/// device-side value.
#[derive(Debug, Deserialize)]
pub struct IoTasksConfig {
    pub tx: Option<bool>,
    pub rx: Option<bool>,
}

impl IoTasksConfig {
    pub fn to_cli_command(&self) -> Result<String, String> {
        if self.tx.is_none() && self.rx.is_none() {
            return Err("at least one of tx or rx must be provided".to_string());
        }
        let mut cmd = "set-io-tasks".to_string();
        if let Some(tx) = self.tx {
            cmd.push_str(&format!(" --tx={}", if tx { "on" } else { "off" }));
        }
        if let Some(rx) = self.rx {
            cmd.push_str(&format!(" --rx={}", if rx { "on" } else { "off" }));
        }
        Ok(cmd)
    }
}

/// `POST /api/devices/{id}/config/csi-delivery` — switch the CSI delivery path and
/// inline log gate. Both fields are independent; either or both may be set.
#[derive(Debug, Deserialize)]
pub struct CsiDeliveryConfig {
    /// `off` | `callback` | `async` | `raw`.
    ///
    /// `raw` enables the zero-copy fast-path; unlike the other modes it is
    /// stored as a flag on the device and only takes effect on the next
    /// `start` (no CSI data is delivered or logged in that mode).
    pub mode: Option<String>,
    /// Toggle for the per-packet UART/JTAG inline log path.
    pub logging: Option<bool>,
}

impl CsiDeliveryConfig {
    pub fn to_cli_command(&self) -> Result<String, String> {
        if self.mode.is_none() && self.logging.is_none() {
            return Err("at least one of mode or logging must be provided".to_string());
        }
        let mut cmd = "set-csi-delivery".to_string();
        if let Some(mode) = &self.mode {
            match mode.as_str() {
                "off" | "callback" | "async" | "raw" => {}
                other => {
                    return Err(format!(
                        "Unknown csi-delivery mode '{other}'; expected off, callback, async, or raw"
                    ));
                }
            }
            cmd.push_str(&format!(" --mode={mode}"));
        }
        if let Some(logging) = self.logging {
            cmd.push_str(&format!(
                " --logging={}",
                if logging { "on" } else { "off" }
            ));
        }
        Ok(cmd)
    }
}

// ─── Output mode ──────────────────────────────────────────────────────────

/// Controls where CSI frames are sent after being read from the serial port.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    /// Stream frames to WebSocket clients only (default).
    #[default]
    Stream,
    /// Write frames to a session dump file only; /api/devices/{id}/ws returns 403.
    Dump,
    /// Both stream to WebSocket clients and write to the dump file.
    Both,
}

#[derive(Debug, Deserialize)]
pub struct OutputModeConfig {
    pub mode: String,
}

// ─── API response ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ApiResponse {
    pub success: bool,
    pub message: String,
}

// ─── Device identification ─────────────────────────────────────────────────

/// Parsed result of the `info` command on `esp-csi-cli-rs`.
///
/// The magic prefix `ESP-CSI-CLI/<version>` is what proves the firmware is
/// `esp-csi-cli-rs`; if the prefix line never arrives, the device is either
/// running unrelated firmware, an older `esp-csi-cli-rs` build that predates
/// the `info` command, or no firmware at all.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    /// The version string from the `ESP-CSI-CLI/<version>` magic line.
    pub banner_version: String,
    /// `name=` line, expected to be `esp-csi-cli-rs`.
    pub name: Option<String>,
    /// `version=` line; should match `banner_version`.
    pub version: Option<String>,
    /// `chip=` line: `esp32` | `esp32c3` | `esp32c5` | `esp32c6` | `esp32s3` | `unknown`.
    pub chip: Option<String>,
    /// `mac=` line — the factory eFuse base MAC (`AA:BB:CC:DD:EE:FF`). On native
    /// USB-Serial-JTAG boards this equals the USB `iSerialNumber` descriptor, so
    /// it is the stable per-board identity the host pins to (see
    /// [`crate::state::DeviceHandle::mac`]). Present from CLI protocol 2 onward.
    pub mac: Option<String>,
    /// `protocol=` line — a wire-format version number bumped on
    /// incompatible grammar changes. Host tooling should refuse unknown
    /// protocol values.
    pub protocol: Option<u32>,
    /// `features=` list (compile-time enabled Cargo features).
    pub features: Vec<String>,
}

/// The firmware CLI protocol this crate's command grammar targets
/// (`CLI_PROTOCOL_VERSION` in `esp-csi-cli-rs`). Firmware reporting a
/// different value — or none at all, which means a pre-protocol-2 build —
/// may silently misinterpret the commands this crate composes, so hosts
/// should treat a mismatch as "reflash required" rather than degrade.
pub const SUPPORTED_CLI_PROTOCOL: u32 = 4;

/// Oldest CLI protocol this crate still drives. Protocol 4 renamed the node-configuration grammar
/// (`--mode=` tokens, `--reporting=`, `set-measurement --periodicity-hz=`) with no aliases, so a
/// protocol 2 or 3 board would reject the commands this crate composes and must be reflashed.
pub const MIN_SUPPORTED_CLI_PROTOCOL: u32 = 4;

impl DeviceInfo {
    /// Whether the firmware speaks a CLI protocol this crate drives
    /// ([`MIN_SUPPORTED_CLI_PROTOCOL`]..=[`SUPPORTED_CLI_PROTOCOL`]). A missing `protocol=` line reads as unsupported: only
    /// pre-protocol-2 firmware omits it, and that grammar predates CLI
    /// protocol 2.
    pub fn protocol_supported(&self) -> bool {
        self.protocol
            .is_some_and(|p| (MIN_SUPPORTED_CLI_PROTOCOL..=SUPPORTED_CLI_PROTOCOL).contains(&p))
    }
}

// ─── Runtime status ───────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct CollectionStatusResponse {
    pub serial_connected: bool,
    pub collection_running: bool,
    pub port_path: String,
}

impl CollectionStatusResponse {
    pub fn from_state(
        serial_connected: &AtomicBool,
        collection_running: &AtomicBool,
        port_path: String,
    ) -> Self {
        Self {
            serial_connected: serial_connected.load(Ordering::SeqCst),
            collection_running: collection_running.load(Ordering::SeqCst),
            port_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::StandardCsiProfile;

    #[test]
    fn measurement_emits_periodicity_only_when_unsolicited_omitted() {
        let cmd = MeasurementConfig {
            periodicity_hz: 100,
            unsolicited: None,
        }
        .to_cli_command();
        assert_eq!(cmd, "set-measurement --periodicity-hz=100");
    }

    #[test]
    fn measurement_emits_unsolicited_flag() {
        let on = MeasurementConfig {
            periodicity_hz: 1000,
            unsolicited: Some(true),
        }
        .to_cli_command();
        assert_eq!(on, "set-measurement --periodicity-hz=1000 --unsolicited=on");
        let off = MeasurementConfig {
            periodicity_hz: 1000,
            unsolicited: Some(false),
        }
        .to_cli_command();
        assert_eq!(off, "set-measurement --periodicity-hz=1000 --unsolicited=off");
    }

    #[test]
    fn measurement_json_uses_periodicity_hz() {
        let cfg: MeasurementConfig = serde_json::from_str(r#"{"periodicity_hz":50}"#).unwrap();
        assert_eq!(cfg.periodicity_hz, 50);
        assert!(serde_json::from_str::<MeasurementConfig>(r#"{"frequency_hz":50}"#).is_err());
    }

    fn wifi(mode: &str, peer_mac: Option<&str>, ht40: Option<&str>) -> WifiConfig {
        WifiConfig {
            mode: mode.to_string(),
            sta_ssid: None,
            sta_password: None,
            ap_ssid: None,
            ap_password: None,
            ap_dhcp: None,
            ap_leases: None,
            ap_burst: None,
            channel: None,
            peer_mac: peer_mac.map(str::to_string),
            ht40: ht40.map(str::to_string),
            emitter_iface: None,
            reporting: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn wifi_emitter_emits_injection_destination() {
        // `--peer-mac` is the emitter's injection destination (unicast to one
        // receiver); the emitter modes take the operating channel too.
        let cmd = wifi("ht20-emitter", Some("AA:BB:CC:DD:EE:FF"), None)
            .to_cli_command(None, &StandardCsiProfile)
            .unwrap();
        assert_eq!(
            cmd,
            "set-wifi --mode=ht20-emitter --set-channel=1 --peer-mac=AA:BB:CC:DD:EE:FF"
        );
    }

    #[test]
    fn wifi_ap_emits_ht40_secondary() {
        // `--ht40` is the softAP secondary channel, i.e. a `wifi-ap` flag.
        let cmd = wifi("wifi-ap", None, Some("above"))
            .to_cli_command(None, &StandardCsiProfile)
            .unwrap();
        assert_eq!(cmd, "set-wifi --mode=wifi-ap --set-channel=1 --ht40=above");
    }

    #[test]
    fn wifi_empty_peer_mac_clears_to_broadcast() {
        let cmd = wifi("ht20-emitter", Some(""), None)
            .to_cli_command(None, &StandardCsiProfile)
            .unwrap();
        assert_eq!(cmd, "set-wifi --mode=ht20-emitter --set-channel=1 --peer-mac=");
    }

    #[test]
    fn wifi_extra_forwards_unknown_flags_verbatim() {
        // Flags belonging to a mode this crate does not name ride through `extra` and must re-emit
        // unquoted. A CSV-valued flag is the case that catches quoting bugs: it has to survive as
        // `a,b,c` rather than `"a,b,c"`.
        //
        // Deliberately generic. Naming a specific out-of-tree flag set here would document a
        // feature this crate does not implement, which is the `extra` map's whole purpose to avoid.
        let mut cfg = wifi("ht20-emitter", None, None);
        cfg.extra
            .insert("example-list".to_string(), serde_json::json!("1,5,9,13"));
        cfg.extra
            .insert("example-ms".to_string(), serde_json::json!(75));
        let cmd = cfg.to_cli_command(None, &StandardCsiProfile).unwrap();
        assert!(cmd.contains("--example-list=1,5,9,13"), "{cmd}");
        assert!(cmd.contains("--example-ms=75"), "{cmd}");
    }

    #[test]
    fn wifi_emitter_emits_inject_period_from_extra() {
        // `--inject-period-ms` is an emitter flag the core does not name; it
        // rides through `extra` like any other unnamed parameter.
        let mut cfg = wifi("ht40-emitter", None, None);
        cfg.extra
            .insert("inject-period-ms".to_string(), serde_json::json!(20));
        let cmd = cfg.to_cli_command(None, &StandardCsiProfile).unwrap();
        assert_eq!(
            cmd,
            "set-wifi --mode=ht40-emitter --set-channel=1 --inject-period-ms=20"
        );
    }

    #[test]
    fn wifi_rejects_malformed_peer_mac() {
        assert!(wifi("ht20-emitter", Some("not-a-mac"), None)
            .to_cli_command(None, &StandardCsiProfile)
            .is_err());
    }

    #[test]
    fn wifi_rejects_bad_ht40() {
        assert!(wifi("wifi-ap", None, Some("sideways"))
            .to_cli_command(None, &StandardCsiProfile)
            .is_err());
    }

    #[test]
    fn wifi_emitter_iface_emits_and_validates() {
        // `--emitter-iface` selects the injection interface. Only `sta` and
        // `ap` exist; anything else is a typo the firmware would reject too.
        for iface in ["sta", "ap"] {
            let mut cfg = wifi("ht20-emitter", None, None);
            cfg.emitter_iface = Some(iface.to_string());
            let cmd = cfg.to_cli_command(None, &StandardCsiProfile).unwrap();
            assert!(cmd.contains(&format!("--emitter-iface={iface}")), "{cmd}");
        }
        let mut cfg = wifi("ht20-emitter", None, None);
        cfg.emitter_iface = Some("both".to_string());
        assert!(cfg.to_cli_command(None, &StandardCsiProfile).is_err());
        // Unset sends nothing: the firmware default (sta) applies.
        let cmd = wifi("ht20-emitter", None, None)
            .to_cli_command(None, &StandardCsiProfile)
            .unwrap();
        assert!(!cmd.contains("emitter-iface"), "{cmd}");
    }

    #[test]
    fn device_info_protocol_support_is_exact() {
        let info = |protocol| DeviceInfo {
            banner_version: "0.7.0".to_string(),
            name: Some("esp-csi-cli-rs".to_string()),
            version: Some("0.7.0".to_string()),
            chip: Some("esp32c6".to_string()),
            mac: None,
            protocol,
            features: Vec::new(),
        };
        assert!(info(Some(SUPPORTED_CLI_PROTOCOL)).protocol_supported());
        assert!(info(Some(4)).protocol_supported());
        // Older and newer grammars are refused: the composed commands
        // may parse differently (or not at all) on either side.
        assert!(!info(Some(1)).protocol_supported());
        assert!(!info(Some(2)).protocol_supported());
        assert!(!info(Some(3)).protocol_supported());
        assert!(!info(Some(5)).protocol_supported());
        // No `protocol=` line means pre-protocol-2 firmware.
        assert!(!info(None).protocol_supported());
    }

    #[test]
    fn wifi_station_forwards_explicit_channel_hint() {
        // An explicit channel in station mode is forwarded as a pre-association
        // band-selection hint.
        let cmd = WifiConfig {
            mode: "station".to_string(),
            sta_ssid: Some("MyNetwork".to_string()),
            sta_password: None,
            ap_ssid: None,
            ap_password: None,
            ap_dhcp: None,
            ap_leases: None,
            ap_burst: None,
            channel: Some(6),
            peer_mac: None,
            ht40: None,
            emitter_iface: None,
            reporting: None,
            extra: BTreeMap::new(),
        }
        .to_cli_command(None, &StandardCsiProfile)
        .unwrap();
        assert_eq!(
            cmd,
            "set-wifi --mode=station --sta-ssid='MyNetwork' --set-channel=6"
        );
    }

    #[test]
    fn wifi_station_omits_channel_when_unset() {
        // Without an explicit channel, station mode sends no `--set-channel`
        // (and never falls back to the chip default) — the firmware derives the
        // channel from the associated AP.
        let cmd = WifiConfig {
            mode: "station".to_string(),
            sta_ssid: Some("MyNetwork".to_string()),
            sta_password: None,
            ap_ssid: None,
            ap_password: None,
            ap_dhcp: None,
            ap_leases: None,
            ap_burst: None,
            channel: None,
            peer_mac: None,
            ht40: None,
            emitter_iface: None,
            reporting: None,
            extra: BTreeMap::new(),
        }
        .to_cli_command(Some("esp32c5"), &StandardCsiProfile)
        .unwrap();
        assert_eq!(cmd, "set-wifi --mode=station --sta-ssid='MyNetwork'");
    }

    #[test]
    fn wifi_ap_c5_defaults_channel_149() {
        let cmd = WifiConfig {
            mode: "wifi-ap".to_string(),
            sta_ssid: None,
            sta_password: None,
            ap_ssid: Some("esp-csi-ap".to_string()),
            ap_password: None,
            ap_dhcp: None,
            ap_leases: None,
            ap_burst: None,
            channel: None,
            peer_mac: None,
            ht40: None,
            emitter_iface: None,
            reporting: None,
            extra: BTreeMap::new(),
        }
        .to_cli_command(Some("esp32c5"), &StandardCsiProfile)
        .unwrap();
        assert_eq!(
            cmd,
            "set-wifi --mode=wifi-ap --ap-ssid='esp-csi-ap' --set-channel=149"
        );
    }

    #[test]
    fn wifi_ap_emits_ap_fields() {
        let cmd = WifiConfig {
            mode: "wifi-ap".to_string(),
            sta_ssid: None,
            sta_password: None,
            ap_ssid: Some("esp-csi-ap".to_string()),
            ap_password: Some(String::new()),
            ap_dhcp: Some(true),
            ap_leases: None,
            ap_burst: None,
            channel: Some(6),
            peer_mac: None,
            ht40: None,
            emitter_iface: None,
            reporting: None,
            extra: BTreeMap::new(),
        }
        .to_cli_command(None, &StandardCsiProfile)
        .unwrap();
        assert_eq!(
            cmd,
            "set-wifi --mode=wifi-ap --ap-ssid='esp-csi-ap' --ap-password='' --ap-dhcp=on --set-channel=6"
        );
    }

    #[test]
    fn wifi_ap_emits_leases_and_burst() {
        let cmd = WifiConfig {
            mode: "wifi-ap".to_string(),
            sta_ssid: None,
            sta_password: None,
            ap_ssid: Some("esp-csi-ap".to_string()),
            ap_password: None,
            ap_dhcp: Some(true),
            ap_leases: Some(4),
            ap_burst: Some(true),
            channel: Some(6),
            peer_mac: None,
            ht40: None,
            emitter_iface: None,
            reporting: None,
            extra: BTreeMap::new(),
        }
        .to_cli_command(None, &StandardCsiProfile)
        .unwrap();
        assert_eq!(
            cmd,
            "set-wifi --mode=wifi-ap --ap-ssid='esp-csi-ap' --ap-dhcp=on --ap-leases=4 --ap-burst=on --set-channel=6"
        );
    }

    #[test]
    fn wifi_ap_burst_off_emits_off() {
        let mut cfg = wifi("wifi-ap", None, None);
        cfg.ap_burst = Some(false);
        let cmd = cfg.to_cli_command(None, &StandardCsiProfile).unwrap();
        assert_eq!(cmd, "set-wifi --mode=wifi-ap --ap-burst=off --set-channel=1");
    }

    #[test]
    fn wifi_ap_rejects_out_of_range_leases() {
        for bad in [0u8, 9] {
            let mut cfg = wifi("wifi-ap", None, None);
            cfg.ap_leases = Some(bad);
            assert!(cfg.to_cli_command(None, &StandardCsiProfile).is_err());
        }
    }

    #[test]
    fn wifi_base_mode_table_is_exactly_the_open_vocabulary() {
        // The base table mirrors the firmware CLI grammar: one entry per operational mode this
        // crate targets, and nothing else. Any mode it does not name — chip-gated, or supplied by
        // a build it does not target — must come in through the profile seam, never this list.
        assert_eq!(
            WIFI_MODES,
            [
                "station",
                "sniffer",
                "wifi-ap",
                "ht20-emitter",
                "ht40-emitter",
                "esp-now-initiator",
                "esp-now-responder",
                "esp-now-simplex-source",
                "esp-now-simplex-peer",
            ]
        );
        for mode in WIFI_MODES {
            assert!(
                wifi(mode, None, None)
                    .to_cli_command(None, &StandardCsiProfile)
                    .is_ok(),
                "base mode {mode} must be accepted"
            );
        }
    }

    #[test]
    fn wifi_rejects_the_retired_mode_tokens() {
        // Hard cut: the pre-802.11bf spellings are not aliases.
        for mode in [
            "esp-now-central",
            "esp-now-peripheral",
            "esp-now-fast-collector",
            "esp-now-fast-source",
        ] {
            assert!(
                wifi(mode, None, None)
                    .to_cli_command(None, &StandardCsiProfile)
                    .is_err(),
                "retired mode {mode} must be rejected"
            );
        }
    }

    #[test]
    fn wifi_rejects_unknown_mode() {
        assert!(wifi("mesh", None, None).to_cli_command(None, &StandardCsiProfile).is_err());
        // "report" is a reporting value, not a mode.
        assert!(wifi("report", None, None)
            .to_cli_command(None, &StandardCsiProfile)
            .is_err());
    }

    #[test]
    fn wifi_station_emits_reporting() {
        let mut cfg = wifi("station", None, None);
        cfg.reporting = Some(Reporting::None);
        let cmd = cfg.to_cli_command(None, &StandardCsiProfile).unwrap();
        assert_eq!(cmd, "set-wifi --mode=station --reporting=none");
    }

    #[test]
    fn wifi_reporting_accepted_where_the_mode_admits_a_choice() {
        for mode in ["station", "wifi-ap", "esp-now-initiator", "esp-now-responder"] {
            for reporting in [Reporting::Report, Reporting::None] {
                let mut cfg = wifi(mode, None, None);
                cfg.reporting = Some(reporting);
                let cmd = cfg.to_cli_command(None, &StandardCsiProfile).unwrap();
                assert!(
                    cmd.ends_with(&format!(" --reporting={}", reporting.as_str())),
                    "{cmd}"
                );
            }
        }
    }

    #[test]
    fn wifi_reporting_rejected_where_the_mode_fixes_it() {
        // The route maps this `Err` to 400.
        for (mode, fixed) in FIXED_REPORTING_MODES {
            assert!(WIFI_MODES.contains(mode), "{mode} is not a base mode");
            for reporting in [Reporting::Report, Reporting::None] {
                let mut cfg = wifi(mode, None, None);
                cfg.reporting = Some(reporting);
                let err = cfg.to_cli_command(None, &StandardCsiProfile).unwrap_err();
                assert!(err.contains(fixed.as_str()), "{err}");
            }
        }
        let mut sniffer = wifi("sniffer", None, None);
        sniffer.reporting = Some(Reporting::None);
        assert_eq!(
            sniffer.to_cli_command(None, &StandardCsiProfile).unwrap_err(),
            "mode 'sniffer' has a fixed reporting (report); omit reporting"
        );
    }

    #[test]
    fn wifi_reporting_omitted_sends_nothing() {
        for mode in WIFI_MODES {
            let cmd = wifi(mode, None, None)
                .to_cli_command(None, &StandardCsiProfile)
                .unwrap();
            assert!(!cmd.contains("--reporting"), "{cmd}");
        }
    }

    #[test]
    fn wifi_reporting_passes_through_profile_modes() {
        let mut cfg = wifi("custom-receiver", None, None);
        cfg.reporting = Some(Reporting::None);
        let cmd = cfg.to_cli_command(None, &ExtraModeProfile).unwrap();
        assert!(cmd.ends_with(" --reporting=none"), "{cmd}");
    }

    #[test]
    fn wifi_reporting_json_is_lowercase_and_optional() {
        let cfg: WifiConfig =
            serde_json::from_str(r#"{"mode":"station","reporting":"none"}"#).unwrap();
        assert_eq!(cfg.reporting, Some(Reporting::None));
        // Not swallowed by the flattened `extra` map.
        assert!(cfg.extra.is_empty());
        let cfg: WifiConfig = serde_json::from_str(r#"{"mode":"station"}"#).unwrap();
        assert_eq!(cfg.reporting, None);
        // The retired values are not aliases.
        for old in ["collector", "listener"] {
            assert!(serde_json::from_str::<WifiConfig>(&format!(
                r#"{{"mode":"station","reporting":"{old}"}}"#
            ))
            .is_err());
        }
        assert_eq!(
            serde_json::to_string(&Reporting::Report).unwrap(),
            r#""report""#
        );
        assert_eq!(serde_json::to_string(&Reporting::None).unwrap(), r#""none""#);
    }

    /// A profile that names an extra pair of modes, standing in for an out-of-tree capability
    /// crate. This crate must accept both and re-emit any unknown `extra` params generically,
    /// without naming either itself.
    struct ExtraModeProfile;
    impl CsiProfile for ExtraModeProfile {
        fn extra_wifi_modes(&self) -> &'static [&'static str] {
            &["custom-emitter", "custom-receiver"]
        }
    }

    #[test]
    fn wifi_accepts_profile_modes_and_emits_extra_params() {
        let mut cfg = wifi("custom-emitter", None, None);
        cfg.extra
            .insert("inject-period-ms".to_string(), serde_json::json!(20));
        let cmd = cfg.to_cli_command(None, &ExtraModeProfile).unwrap();
        assert_eq!(
            cmd,
            "set-wifi --mode=custom-emitter --set-channel=1 --inject-period-ms=20"
        );
        for mode in ExtraModeProfile.extra_wifi_modes() {
            // The seam carries every mode it names …
            assert!(wifi(mode, None, None)
                .to_cli_command(None, &ExtraModeProfile)
                .is_ok());
            // … and none of them leaks into the open base table.
            assert!(!WIFI_MODES.contains(mode));
            assert!(wifi(mode, None, None)
                .to_cli_command(None, &StandardCsiProfile)
                .is_err());
        }
    }

    #[test]
    fn wifi_error_lists_base_table_and_profile_modes() {
        let err = wifi("mesh", None, None)
            .to_cli_command(None, &ExtraModeProfile)
            .unwrap_err();
        assert_eq!(
            err,
            "Unknown wifi mode 'mesh'; expected one of: station, sniffer, \
             wifi-ap, ht20-emitter, ht40-emitter, esp-now-initiator, \
             esp-now-responder, esp-now-simplex-source, esp-now-simplex-peer, \
             custom-emitter, custom-receiver"
        );
    }

    /// A `CsiConfig` with every named flag unset and an empty `extra` map.
    fn csi_cfg() -> CsiConfig {
        CsiConfig {
            lltf: None,
            htltf: None,
            stbc_htltf: None,
            ltf_merge: None,
            csi: None,
            csi_legacy: None,
            csi_ht20: None,
            csi_ht40: None,
            dump_ack: None,
            csi_force_lltf: None,
            csi_vht: None,
            val_scale_cfg: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn csi_emits_on_off_toggles() {
        let mut cfg = csi_cfg();
        cfg.lltf = Some(false);
        cfg.csi = Some(true);
        cfg.csi_legacy = Some(false);
        cfg.dump_ack = Some(false);
        let cmd = cfg.to_cli_command(&StandardCsiProfile).unwrap();
        assert_eq!(cmd, "set-csi --lltf=off --csi=on --csi-legacy=off --dump-ack=off");
    }

    #[test]
    fn csi_emits_default_preset() {
        let mut cfg = csi_cfg();
        cfg.extra
            .insert("preset".to_string(), serde_json::json!("default"));
        let cmd = cfg.to_cli_command(&StandardCsiProfile).unwrap();
        assert_eq!(cmd, "set-csi --preset=default");
    }

    #[test]
    fn csi_emits_extra_flags_generically() {
        // Unknown acquisition flags round-trip through the flattened `extra`
        // map, formatted with the same on/off + numeric conventions.
        let mut cfg = csi_cfg();
        cfg.csi = Some(true);
        cfg.extra
            .insert("csi-su".to_string(), serde_json::json!(1));
        cfg.extra
            .insert("csi-beamformed".to_string(), serde_json::json!(true));
        let cmd = cfg.to_cli_command(&StandardCsiProfile).unwrap();
        assert_eq!(
            cmd,
            "set-csi --csi=on --csi-beamformed=on --csi-su=1"
        );
    }

    #[test]
    fn csi_rejects_unknown_preset() {
        let mut cfg = csi_cfg();
        cfg.extra
            .insert("preset".to_string(), serde_json::json!("turbo"));
        assert!(cfg.to_cli_command(&StandardCsiProfile).is_err());
    }

    #[test]
    fn csi_output_emits_enabled_boolean() {
        assert_eq!(
            CsiOutputConfig { enabled: true }.to_cli_command(),
            "set-csi-output --enabled=true"
        );
        assert_eq!(
            CsiOutputConfig { enabled: false }.to_cli_command(),
            "set-csi-output --enabled=false"
        );
    }

    #[test]
    fn csi_output_rejects_the_retired_collection_mode_body() {
        let cfg: CsiOutputConfig = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
        assert!(!cfg.enabled);
        // `{"mode":"collector"}` was an old body shape. It must fail to
        // deserialize rather than be silently accepted as some default.
        assert!(serde_json::from_str::<CsiOutputConfig>(r#"{"mode":"collector"}"#).is_err());
        assert!(serde_json::from_str::<CsiOutputConfig>(r#"{"mode":"listener"}"#).is_err());
    }

    #[test]
    fn firmware_defaults_capture_and_deliver() {
        let defaults = DeviceConfig::firmware_defaults();
        // The default node reports, on the promiscuous capture path …
        assert_eq!(defaults.wifi.mode.as_deref(), Some("sniffer"));
        assert!(WIFI_MODES.contains(&defaults.wifi.mode.as_deref().unwrap()));
        // … delivering what it captures, with no unicast injection destination.
        assert_eq!(defaults.collection.csi_output_enabled, Some(true));
        assert_eq!(defaults.wifi.peer_mac.as_deref(), Some("broadcast"));
    }

    #[test]
    fn csi_delivery_accepts_raw() {
        let cmd = CsiDeliveryConfig {
            mode: Some("raw".to_string()),
            logging: None,
        }
        .to_cli_command()
        .unwrap();
        assert_eq!(cmd, "set-csi-delivery --mode=raw");
    }

    #[test]
    fn csi_delivery_rejects_unknown_mode() {
        assert!(CsiDeliveryConfig {
            mode: Some("bogus".to_string()),
            logging: None,
        }
        .to_cli_command()
        .is_err());
    }

    #[test]
    fn protocol_emits_lowercased_command() {
        let cmd = ProtocolConfig {
            protocol: "AC".to_string(),
        }
        .to_cli_command(&StandardCsiProfile)
        .unwrap();
        assert_eq!(cmd, "set-protocol --protocol=ac");
    }

    #[test]
    fn protocol_accepts_all_valid_values() {
        for p in ["b", "g", "n", "lr", "a", "ac"] {
            assert!(ProtocolConfig {
                protocol: p.to_string(),
            }
            .to_cli_command(&StandardCsiProfile)
            .is_ok());
        }
    }

    #[test]
    fn protocol_rejects_unknown_value() {
        assert!(ProtocolConfig {
            protocol: "wifi7".to_string(),
        }
        .to_cli_command(&StandardCsiProfile)
        .is_err());
    }
}
