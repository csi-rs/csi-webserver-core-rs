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
/// matching `POST /api/config/*` endpoint succeeds, and reset to firmware
/// defaults by `POST /api/config/reset`. Values can drift if the device is
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
    /// `node_mode` — `sniffer` | `station` | `wifi-ap` | `esp-now-central` |
    /// `esp-now-peripheral` | `esp-now-fast-collector` | `esp-now-fast-source`.
    pub mode: Option<String>,
    /// `channel` — `u8`. Valid Wi-Fi 2.4 GHz: 1..=14.
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
    /// `peer_mac` — explicit ESP-NOW source-MAC filter, or `auto` for the
    /// default magic-prefix pairing. ESP-NOW modes only.
    pub peer_mac: Option<String>,
    /// `ht40_secondary` — forced ESP-NOW TX secondary channel:
    /// `above` | `below` | `none` (HT20/legacy). ESP-NOW modes only.
    pub ht40: Option<String>,
}

/// `[Collection]` section in `show-config`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CollectionSection {
    /// `collection_mode` — `collector` | `listener`.
    pub mode: Option<String>,
    /// `trigger_freq` Hz. `0` disables traffic generation.
    pub traffic_hz: Option<u64>,
    /// `flood_unsolicited` — ICMP flood sends unsolicited echo replies
    /// (one-directional traffic) instead of echo requests.
    pub unsolicited: Option<bool>,
    /// `phy_rate` enum — e.g. `mcs0-lgi`. Honored by all modes except `station`.
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
/// flag; they are populated by `POST /api/config/reset` from firmware defaults
/// but otherwise stay fixed.
///
/// Any acquisition flag the core does not name (supplied by an embedder's
/// [`CsiProfile`](crate::profile::CsiProfile) or a profile-aware client) is
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
    /// the cache by `POST /api/config/reset` so the response after a
    /// reset reflects what the firmware actually holds, even before the
    /// user re-sends any `set-*` commands.
    ///
    /// Both the classic and the extended (C5/C6) CSI defaults are populated —
    /// the caller can ignore the fields irrelevant to the connected chip
    /// (consult `GET /api/info` for the `chip` field).
    pub fn firmware_defaults() -> Self {
        Self::firmware_defaults_for_chip(None)
    }

    /// Like [`firmware_defaults`], with chip-specific Wi-Fi channel (C5 → 149,
    /// C6 → 6, others → 1) matching the esp-csi-cli-rs lab examples.
    pub fn firmware_defaults_for_chip(chip: Option<&str>) -> Self {
        let defaults = Self {
            wifi: WifiSection {
                mode: Some("sniffer".to_string()),
                channel: Some(default_wifi_channel(chip)),
                sta_ssid: Some(String::new()),
                ap_ssid: Some("esp-csi-ap".to_string()),
                ap_password: None,
                ap_dhcp: Some(true),
                ap_leases: Some(4),
                ap_burst: Some(false),
                peer_mac: Some("auto".to_string()),
                ht40: Some("none".to_string()),
            },
            collection: CollectionSection {
                mode: Some("collector".to_string()),
                traffic_hz: Some(100),
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
        };
        defaults
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

/// Validate an ESP-NOW peer MAC the way the firmware does: six hex octets
/// separated by `:` or `-` (case-insensitive). An empty string is *valid* and
/// means "clear back to auto".
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

/// Accepted `set-wifi --mode=` values (esp-csi-cli-rs v0.7.0).
const WIFI_MODES: &[&str] = &[
    "station",
    "sniffer",
    "wifi-ap",
    "esp-now-central",
    "esp-now-peripheral",
    "esp-now-fast-collector",
    "esp-now-fast-source",
];

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
    /// ESP-NOW peer source MAC (`aa:bb:cc:dd:ee:ff` or `aa-bb-...`). An empty
    /// string clears the filter back to automatic magic-prefix pairing.
    /// ESP-NOW modes only; ignored by the firmware in other modes.
    pub peer_mac: Option<String>,
    /// Forced ESP-NOW TX HT40 secondary channel: `above` | `below` |
    /// `none` | `off`. ESP-NOW modes only.
    pub ht40: Option<String>,
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
            // firmware clears the filter back to auto.
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

        // Profile-supplied / unknown params (e.g. an injector's inter-frame
        // period) ride in `extra` and re-emit generically as `--{key}={value}`.
        for (key, value) in &self.extra {
            push_extra(&mut cmd, key, value);
        }

        Ok(cmd)
    }
}

#[derive(Debug, Deserialize)]
pub struct TrafficConfig {
    /// Traffic generation frequency in Hz; `0` disables generation.
    pub frequency_hz: u64,
    /// `--unsolicited=on|off` (default off). When on, the ICMP flood sends
    /// unsolicited echo *replies* instead of echo requests: the peer silently
    /// ignores them at the IP level, so traffic is strictly one-directional —
    /// stable offered rate, but the flooding node captures no CSI back from
    /// replies. Omitted → flag not forwarded, firmware keeps its default.
    pub unsolicited: Option<bool>,
}

impl TrafficConfig {
    pub fn to_cli_command(&self) -> String {
        let mut cmd = format!("set-traffic --frequency-hz={}", self.frequency_hz);
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
/// [`CsiProfile`](crate::profile::CsiProfile) or a profile-aware client can drive
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
    /// active [`CsiProfile`](crate::profile::CsiProfile)). Each entry other
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
        // Carry unknown flags into the cache verbatim so `GET /api/config`
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

#[derive(Debug, Deserialize)]
pub struct CollectionModeConfig {
    /// `collector` or `listener`.
    pub mode: String,
}

impl CollectionModeConfig {
    pub fn to_cli_command(&self) -> Result<String, String> {
        match self.mode.as_str() {
            "collector" | "listener" => {
                Ok(format!("set-collection-mode --mode={}", self.mode))
            }
            other => Err(format!(
                "Unknown collection mode '{other}'; expected collector or listener"
            )),
        }
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

/// `POST /api/config/rate` — pin the Wi-Fi PHY rate (honored by all modes
/// except `station` on the firmware side).
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

/// `POST /api/config/protocol` — set the Wi-Fi PHY protocol applied to the node
/// at the start of each collection run.
#[derive(Debug, Deserialize)]
pub struct ProtocolConfig {
    /// One of `b` | `g` | `n` | `lr` | `a` | `ac`, plus any value the active
    /// [`CsiProfile`](crate::profile::CsiProfile) advertises.
    pub protocol: String,
}

impl ProtocolConfig {
    /// Accepted protocol values, matching the firmware's `set-protocol` parser.
    const VALID: &[&str] = &["b", "g", "n", "lr", "a", "ac"];

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

/// `POST /api/config/io-tasks` — toggle the per-direction TX/RX Embassy tasks.
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

/// `POST /api/config/csi-delivery` — switch the CSI delivery path and
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
    /// Write frames to a session dump file only; /api/ws returns 403.
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
    fn traffic_emits_frequency_only_when_unsolicited_omitted() {
        let cmd = TrafficConfig {
            frequency_hz: 100,
            unsolicited: None,
        }
        .to_cli_command();
        assert_eq!(cmd, "set-traffic --frequency-hz=100");
    }

    #[test]
    fn traffic_emits_unsolicited_flag() {
        let on = TrafficConfig {
            frequency_hz: 1000,
            unsolicited: Some(true),
        }
        .to_cli_command();
        assert_eq!(on, "set-traffic --frequency-hz=1000 --unsolicited=on");
        let off = TrafficConfig {
            frequency_hz: 1000,
            unsolicited: Some(false),
        }
        .to_cli_command();
        assert_eq!(off, "set-traffic --frequency-hz=1000 --unsolicited=off");
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
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn wifi_emits_peer_mac_and_ht40() {
        let cmd = wifi("esp-now-central", Some("AA:BB:CC:DD:EE:FF"), Some("above"))
            .to_cli_command(None, &StandardCsiProfile)
            .unwrap();
        assert_eq!(
            cmd,
            "set-wifi --mode=esp-now-central --set-channel=1 --peer-mac=AA:BB:CC:DD:EE:FF --ht40=above"
        );
    }

    #[test]
    fn wifi_empty_peer_mac_clears_to_auto() {
        let cmd = wifi("esp-now-peripheral", Some(""), None)
            .to_cli_command(None, &StandardCsiProfile)
            .unwrap();
        assert_eq!(cmd, "set-wifi --mode=esp-now-peripheral --set-channel=1 --peer-mac=");
    }

    #[test]
    fn wifi_extra_forwards_hop_params_verbatim() {
        // Channel-hopping flags (--hop-list / --hop-channel / --hop-burst /
        // --hop-follow-ms) are HE20-node keys the open core does not name; they
        // ride through `extra` and must re-emit unquoted — the CSV hop list in
        // particular must survive as `1,5,9,13`, not `"1,5,9,13"`.
        let mut cfg = wifi("esp-now-central", None, None);
        cfg.extra
            .insert("hop-list".to_string(), serde_json::json!("1,5,9,13"));
        cfg.extra
            .insert("hop-follow-ms".to_string(), serde_json::json!(75));
        let cmd = cfg.to_cli_command(None, &StandardCsiProfile).unwrap();
        assert!(cmd.contains("--hop-list=1,5,9,13"), "{cmd}");
        assert!(cmd.contains("--hop-follow-ms=75"), "{cmd}");
    }

    #[test]
    fn wifi_rejects_malformed_peer_mac() {
        assert!(wifi("esp-now-central", Some("not-a-mac"), None)
            .to_cli_command(None, &StandardCsiProfile)
            .is_err());
    }

    #[test]
    fn wifi_rejects_bad_ht40() {
        assert!(wifi("esp-now-central", None, Some("sideways"))
            .to_cli_command(None, &StandardCsiProfile)
            .is_err());
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
    fn wifi_fast_collector_emits_peer_mac_and_ht40() {
        let cmd = wifi("esp-now-fast-collector", Some("aa:bb:cc:dd:ee:ff"), Some("below"))
            .to_cli_command(None, &StandardCsiProfile)
            .unwrap();
        assert_eq!(
            cmd,
            "set-wifi --mode=esp-now-fast-collector --set-channel=1 --peer-mac=aa:bb:cc:dd:ee:ff --ht40=below"
        );
    }

    #[test]
    fn wifi_rejects_unknown_mode() {
        assert!(wifi("mesh", None, None).to_cli_command(None, &StandardCsiProfile).is_err());
    }

    /// A profile that names an extra wifi mode, standing in for an out-of-tree
    /// capability crate. The open core must accept the mode and re-emit any
    /// unknown `extra` params generically, without naming either itself.
    struct ExtraModeProfile;
    impl CsiProfile for ExtraModeProfile {
        fn extra_wifi_modes(&self) -> &'static [&'static str] {
            &["custom-mode"]
        }
    }

    #[test]
    fn wifi_accepts_profile_mode_and_emits_extra_params() {
        let mut cfg = wifi("custom-mode", None, None);
        cfg.extra
            .insert("inject-period-ms".to_string(), serde_json::json!(20));
        let cmd = cfg.to_cli_command(None, &ExtraModeProfile).unwrap();
        assert_eq!(
            cmd,
            "set-wifi --mode=custom-mode --set-channel=1 --inject-period-ms=20"
        );
        // The standard (no-op) profile does not name the mode, so it is rejected.
        assert!(wifi("custom-mode", None, None)
            .to_cli_command(None, &StandardCsiProfile)
            .is_err());
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
