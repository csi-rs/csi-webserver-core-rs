//! Host-side decoder for the firmware's `serialized` CSI output.
//!
//! ## Wire format (esp-csi-rs 0.12 and later)
//! Each frame is the radio-neutral [`wire`](crate::wire) contract: a postcard [`Envelope`] followed
//! by a [`Body`], in one COBS frame. The module is vendored from esp-csi-rs, so the definitions are
//! the firmware's own rather than a hand-kept mirror. The envelope carries a format version that
//! [`decode_frame`] checks before the body, so a frame from a newer firmware is reported as
//! [`DecodeError::UnsupportedVersion`] instead of mis-read.
//!
//! ## Older firmware (esp-csi-rs 0.11 and earlier)
//! Those builds emitted a chip-specific `CSIDataPacket` with no header. When a frame does not decode
//! as the wire format, [`decode_frame`] falls back to those layouts ([`PacketA`], [`PacketBc5`],
//! [`PacketBc6`]), chosen by the chip the firmware's `info` exchange reported, so a fleet running
//! mixed firmware keeps decoding.
//!
//! Either way the result is a [`DecodedCsi`]: the superset record the Parquet sink and downstream
//! consumers read.

use serde::{Deserialize, Serialize};

use crate::wire::{self, Body, CsiPayload, Envelope, SessionInfo, Stimulus, VendorRx};

/// Which on-device `CSIDataPacket` layout a connected chip produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipVariant {
    /// esp32, esp32c3, esp32s3 — the full radio-metadata layout ([`PacketA`]).
    Esp32Family,
    /// esp32c5 — the reduced layout without the c6-only fields ([`PacketBc5`]).
    Esp32c5,
    /// esp32c6 — the reduced layout plus `sigb_len`/`cur_single_mpdu`/`rxmatch0`.
    Esp32c6,
}

impl ChipVariant {
    /// Map a firmware `chip=` string (case-insensitive) to its wire layout.
    ///
    /// Returns `None` for unrecognized chips so the caller can refuse to decode
    /// rather than guess a layout.
    pub fn from_chip_str(chip: &str) -> Option<Self> {
        match chip.trim().to_ascii_lowercase().as_str() {
            "esp32" | "esp32c3" | "esp32s3" | "esp32s2" => Some(Self::Esp32Family),
            "esp32c5" => Some(Self::Esp32c5),
            "esp32c6" => Some(Self::Esp32c6),
            _ => None,
        }
    }
}

/// Optional NTP-derived calendar timestamp the firmware may attach to a packet.
///
/// Mirror of `esp_csi_rs::time::DateTime` (all fields `u64`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateTime {
    pub year: u64,
    pub month: u64,
    pub day: u64,
    pub hour: u64,
    pub minute: u64,
    pub second: u64,
    pub millisecond: u64,
}

/// Compact CSI data-format descriptor.
///
/// Mirror of `esp_csi_rs::csi::RxCSIFmt` — **variant order is the wire encoding**
/// (postcard encodes the discriminant as a varint of the declaration index), so
/// do not reorder.
// `Default` is `Undefined`, and the attribute is on that variant rather than reordering anything:
// variant order IS the wire encoding (postcard encodes the discriminant as a varint of the
// declaration index), so `#[default]` marks the existing variant in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RxCsiFmt {
    Bw20,
    HtBw20,
    HtBw20Stbc,
    SecbBw20,
    SecbHtBw20,
    SecbHtBw20Stbc,
    SecbHtBw40,
    SecbHtBw40Stbc,
    SecaBw20,
    SecaHtBw20,
    SecaHtBw20Stbc,
    SecaHtBw40,
    SecaHtBw40Stbc,
    /// VHT 20 MHz (`cur_bb_format == 3` on C5/C6).
    VhtBw20,
    /// Any format this build does not name (e.g. a `cur_bb_format` ≥ 4 the open
    /// firmware leaves unlabelled). The raw `cur_bb_format` is preserved on the
    /// decoded record, so an embedder's
    /// [`CsiProfile`](crate::profile::CsiProfile) can label it downstream.
    #[default]
    Undefined,
}

impl RxCsiFmt {
    /// Stable lowercase-ish identifier for the Parquet `data_format` column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bw20 => "Bw20",
            Self::HtBw20 => "HtBw20",
            Self::HtBw20Stbc => "HtBw20Stbc",
            Self::SecbBw20 => "SecbBw20",
            Self::SecbHtBw20 => "SecbHtBw20",
            Self::SecbHtBw20Stbc => "SecbHtBw20Stbc",
            Self::SecbHtBw40 => "SecbHtBw40",
            Self::SecbHtBw40Stbc => "SecbHtBw40Stbc",
            Self::SecaBw20 => "SecaBw20",
            Self::SecaHtBw20 => "SecaHtBw20",
            Self::SecaHtBw20Stbc => "SecaHtBw20Stbc",
            Self::SecaHtBw40 => "SecaHtBw40",
            Self::SecaHtBw40Stbc => "SecaHtBw40Stbc",
            Self::VhtBw20 => "VhtBw20",
            Self::Undefined => "Undefined",
        }
    }
}

/// esp32 / esp32c3 / esp32s3 layout — mirror of `CSIDataPacket`
/// (`#[cfg(not(any(esp32c5, esp32c6)))]`). Field order is the wire order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketA {
    pub mac: [u8; 6],
    pub rssi: i32,
    pub timestamp: u32,
    pub rate: u32,
    pub sgi: u32,
    pub secondary_channel: u32,
    pub channel: u32,
    pub bandwidth: u32,
    pub antenna: u32,
    pub sig_mode: u32,
    pub mcs: u32,
    pub smoothing: u32,
    pub not_sounding: u32,
    pub aggregation: u32,
    pub stbc: u32,
    pub fec_coding: u32,
    pub ampdu_cnt: u32,
    pub noise_floor: i32,
    pub rx_state: u32,
    pub sig_len: u32,
    pub date_time: Option<DateTime>,
    pub sequence_number: u16,
    pub data_format: RxCsiFmt,
    pub csi_data_len: u16,
    pub csi_data: Vec<i8>,
}

/// esp32c5 layout — mirror of the `#[cfg(any(esp32c5, esp32c6))]` `CSIDataPacket`
/// **without** the `#[cfg(feature = "esp32c6")]` fields. Field order is the wire
/// order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketBc5 {
    pub mac: [u8; 6],
    pub rssi: i32,
    pub timestamp: u32,
    pub rate: u32,
    pub noise_floor: i32,
    pub sig_len: u32,
    pub rx_state: u32,
    pub dump_len: u32,
    pub cur_bb_format: u32,
    pub rx_channel_estimate_info_vld: u32,
    pub rx_channel_estimate_len: u32,
    pub second: u32,
    pub channel: u32,
    pub is_group: u32,
    pub rxend_state: u32,
    pub rxmatch3: u32,
    pub rxmatch2: u32,
    pub rxmatch1: u32,
    pub date_time: Option<DateTime>,
    pub sequence_number: u16,
    pub csi_data_len: u16,
    pub data_format: RxCsiFmt,
    pub csi_data: Vec<i8>,
}

/// esp32c6 layout — the c5 layout plus the three `#[cfg(feature = "esp32c6")]`
/// fields (`sigb_len`, `cur_single_mpdu`, `rxmatch0`) at their declared
/// positions. Field order is the wire order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketBc6 {
    pub mac: [u8; 6],
    pub rssi: i32,
    pub timestamp: u32,
    pub rate: u32,
    pub noise_floor: i32,
    pub sig_len: u32,
    pub rx_state: u32,
    pub dump_len: u32,
    pub sigb_len: u32,
    pub cur_single_mpdu: u32,
    pub cur_bb_format: u32,
    pub rx_channel_estimate_info_vld: u32,
    pub rx_channel_estimate_len: u32,
    pub second: u32,
    pub channel: u32,
    pub is_group: u32,
    pub rxend_state: u32,
    pub rxmatch3: u32,
    pub rxmatch2: u32,
    pub rxmatch1: u32,
    pub rxmatch0: u32,
    pub date_time: Option<DateTime>,
    pub sequence_number: u16,
    pub csi_data_len: u16,
    pub data_format: RxCsiFmt,
    pub csi_data: Vec<i8>,
}

/// Chip-agnostic decoded CSI record — the superset of every layout's fields.
///
/// Fields absent on the source chip are `None`. This is what the Parquet sink
/// consumes; its column set is the union of all chip layouts plus the
/// host-supplied receive time.
#[derive(Debug, Clone, Default)]
pub struct DecodedCsi {
    // ── Common to every layout ──────────────────────────────────────────
    pub mac: [u8; 6],
    pub rssi: i32,
    pub timestamp: u32,
    pub rate: u32,
    pub noise_floor: i32,
    pub sig_len: u32,
    pub rx_state: u32,
    pub channel: u32,
    pub date_time: Option<DateTime>,
    pub sequence_number: u16,
    pub data_format: RxCsiFmt,
    pub csi_data_len: u16,
    pub csi_data: Vec<i8>,

    // ── esp32-family only ───────────────────────────────────────────────
    pub sgi: Option<u32>,
    pub secondary_channel: Option<u32>,
    pub bandwidth: Option<u32>,
    pub antenna: Option<u32>,
    pub sig_mode: Option<u32>,
    pub mcs: Option<u32>,
    pub smoothing: Option<u32>,
    pub not_sounding: Option<u32>,
    pub aggregation: Option<u32>,
    pub stbc: Option<u32>,
    pub fec_coding: Option<u32>,
    pub ampdu_cnt: Option<u32>,

    // ── c5 / c6 only ────────────────────────────────────────────────────
    pub dump_len: Option<u32>,
    pub cur_bb_format: Option<u32>,
    pub rx_channel_estimate_info_vld: Option<u32>,
    pub rx_channel_estimate_len: Option<u32>,
    pub second: Option<u32>,
    pub is_group: Option<u32>,
    pub rxend_state: Option<u32>,
    pub rxmatch3: Option<u32>,
    pub rxmatch2: Option<u32>,
    pub rxmatch1: Option<u32>,

    // ── c6 only ─────────────────────────────────────────────────────────
    pub sigb_len: Option<u32>,
    pub cur_single_mpdu: Option<u32>,
    pub rxmatch0: Option<u32>,

    // ── Wire format (esp-csi-rs 0.12+); `None` for older firmware and array-list lines ──
    /// Wire format version of the frame.
    pub wire_version: Option<u8>,
    /// Delivering node's identity (its base MAC).
    pub node_id: Option<[u8; 6]>,
    /// Measurement session the frame belongs to.
    pub session_id: Option<u32>,
    /// Per-run frame counter; a gap is a frame lost between node and host.
    pub stream_seq: Option<u32>,
    /// Kind of measurement source.
    pub source: Option<wire::SourceKind>,
    /// Receive time on the node's clock, microseconds, 64-bit (does not wrap).
    pub timestamp_us: Option<u64>,
    /// Format of the measured PPDU.
    pub ppdu: Option<wire::PpduFormat>,
    /// Bandwidth of the measured PPDU, MHz.
    pub bandwidth_mhz: Option<u16>,
    /// How the raw CSI buffer maps onto subcarriers.
    pub layout: Option<wire::LayoutId>,
    /// The raw buffer's first four bytes are invalid.
    pub first_word_invalid: Option<bool>,
    /// Receive chains / spatial streams.
    pub n_rx: Option<u8>,
    pub n_ss: Option<u8>,
    /// What excited the channel.
    pub stimulus: Option<Stimulus>,
    /// The measured MPDU's header digest.
    pub header: Option<wire::HeaderDigest>,
    /// Score of a threshold-policy variation-only report.
    pub variation: Option<u16>,
    /// A grouped (802.11bf-shaped) report, kept as delivered.
    pub grouped: Option<GroupedReport>,
}

/// A [`CsiPayload::Grouped`] report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupedReport {
    pub ng: u8,
    pub nb: u8,
    pub sc_start: i16,
    pub n_sc: u16,
    pub n_rx: u8,
    pub n_tx: u8,
    pub data: Vec<u8>,
}

impl DecodedCsi {
    /// Subcarrier indices of `csi_data`, one per `(imag, real)` pair, when the layout is known.
    pub fn subcarrier_indices(&self) -> Option<Vec<i16>> {
        let layout = self.layout?;
        if layout.byte_len() != Some(self.csi_data.len()) {
            return None;
        }
        Some(layout.indices().map(|(_, i)| i).collect())
    }

    /// Frequency offset from the channel centre of each subcarrier in `csi_data`, Hz.
    pub fn subcarrier_freqs_hz(&self) -> Option<Vec<i32>> {
        let layout = self.layout?;
        if layout.byte_len() != Some(self.csi_data.len()) {
            return None;
        }
        Some(
            layout
                .indices()
                .map(|(ltf, i)| i as i32 * ltf.spacing_hz() as i32)
                .collect(),
        )
    }
}

impl From<PacketA> for DecodedCsi {
    fn from(p: PacketA) -> Self {
        DecodedCsi {
            mac: p.mac,
            rssi: p.rssi,
            timestamp: p.timestamp,
            rate: p.rate,
            noise_floor: p.noise_floor,
            sig_len: p.sig_len,
            rx_state: p.rx_state,
            channel: p.channel,
            date_time: p.date_time,
            sequence_number: p.sequence_number,
            data_format: p.data_format,
            csi_data_len: p.csi_data_len,
            csi_data: p.csi_data,
            sgi: Some(p.sgi),
            secondary_channel: Some(p.secondary_channel),
            bandwidth: Some(p.bandwidth),
            antenna: Some(p.antenna),
            sig_mode: Some(p.sig_mode),
            mcs: Some(p.mcs),
            smoothing: Some(p.smoothing),
            not_sounding: Some(p.not_sounding),
            aggregation: Some(p.aggregation),
            stbc: Some(p.stbc),
            fec_coding: Some(p.fec_coding),
            ampdu_cnt: Some(p.ampdu_cnt),
            dump_len: None,
            cur_bb_format: None,
            rx_channel_estimate_info_vld: None,
            rx_channel_estimate_len: None,
            second: None,
            is_group: None,
            rxend_state: None,
            rxmatch3: None,
            rxmatch2: None,
            rxmatch1: None,
            sigb_len: None,
            cur_single_mpdu: None,
            rxmatch0: None,
            ..Default::default()
        }
    }
}

impl From<PacketBc5> for DecodedCsi {
    fn from(p: PacketBc5) -> Self {
        DecodedCsi {
            mac: p.mac,
            rssi: p.rssi,
            timestamp: p.timestamp,
            rate: p.rate,
            noise_floor: p.noise_floor,
            sig_len: p.sig_len,
            rx_state: p.rx_state,
            channel: p.channel,
            date_time: p.date_time,
            sequence_number: p.sequence_number,
            data_format: p.data_format,
            csi_data_len: p.csi_data_len,
            csi_data: p.csi_data,
            sgi: None,
            secondary_channel: None,
            bandwidth: None,
            antenna: None,
            sig_mode: None,
            mcs: None,
            smoothing: None,
            not_sounding: None,
            aggregation: None,
            stbc: None,
            fec_coding: None,
            ampdu_cnt: None,
            dump_len: Some(p.dump_len),
            cur_bb_format: Some(p.cur_bb_format),
            rx_channel_estimate_info_vld: Some(p.rx_channel_estimate_info_vld),
            rx_channel_estimate_len: Some(p.rx_channel_estimate_len),
            second: Some(p.second),
            is_group: Some(p.is_group),
            rxend_state: Some(p.rxend_state),
            rxmatch3: Some(p.rxmatch3),
            rxmatch2: Some(p.rxmatch2),
            rxmatch1: Some(p.rxmatch1),
            sigb_len: None,
            cur_single_mpdu: None,
            rxmatch0: None,
            ..Default::default()
        }
    }
}

impl From<PacketBc6> for DecodedCsi {
    fn from(p: PacketBc6) -> Self {
        DecodedCsi {
            mac: p.mac,
            rssi: p.rssi,
            timestamp: p.timestamp,
            rate: p.rate,
            noise_floor: p.noise_floor,
            sig_len: p.sig_len,
            rx_state: p.rx_state,
            channel: p.channel,
            date_time: p.date_time,
            sequence_number: p.sequence_number,
            data_format: p.data_format,
            csi_data_len: p.csi_data_len,
            csi_data: p.csi_data,
            sgi: None,
            secondary_channel: None,
            bandwidth: None,
            antenna: None,
            sig_mode: None,
            mcs: None,
            smoothing: None,
            not_sounding: None,
            aggregation: None,
            stbc: None,
            fec_coding: None,
            ampdu_cnt: None,
            dump_len: Some(p.dump_len),
            cur_bb_format: Some(p.cur_bb_format),
            rx_channel_estimate_info_vld: Some(p.rx_channel_estimate_info_vld),
            rx_channel_estimate_len: Some(p.rx_channel_estimate_len),
            second: Some(p.second),
            is_group: Some(p.is_group),
            rxend_state: Some(p.rxend_state),
            rxmatch3: Some(p.rxmatch3),
            rxmatch2: Some(p.rxmatch2),
            rxmatch1: Some(p.rxmatch1),
            sigb_len: Some(p.sigb_len),
            cur_single_mpdu: Some(p.cur_single_mpdu),
            rxmatch0: Some(p.rxmatch0),
            ..Default::default()
        }
    }
}

/// Parse one `array-list` line into the same record the binary path produces.
///
/// ## Why this exists
///
/// `serialized` mode puts raw COBS on the wire on a text build and wraps the identical frame in
/// `defmt::println!("{=[u8]}")` on a defmt build, so a defmt board cannot use it — the bytes arrive
/// inside a defmt frame and the postcard decoder discards them. `array-list` is the compact form
/// that survives, because it is a complete ASCII line per packet. It is therefore the mode the pool
/// asks a defmt board for (`Pool::log_mode_for`), and this is what reads it.
///
/// ## Layout
///
/// The firmware writes `[seq,rssi,rate,noise_floor,channel,timestamp,sig_len,rx_state,` then a
/// chip-specific run of diagnostic counters, then `sig_len,csi_data_len,[i0,q0,…]]`.
///
/// Parsed WITHOUT a per-chip position table, deliberately. The first eight fields are common to
/// every layout, and `csi_data_len` is always the last header field before the payload — so the
/// values that matter are addressable from the two ends, and the chip-specific middle (which is
/// diagnostics) is skipped rather than guessed. A table indexed by chip would be a second place for
/// the layout to be wrong, and it would go stale the next time a field is added for one part.
///
/// ## Transmission identity
///
/// Both halves of it survive. `frame_seq` is the leading `sequence_number` field — the pool maps
/// `frame_seq: Some(decoded.sequence_number)` — and `mac` arrives in the tail the firmware appends
/// after the payload. So an array-list capture can be joined on `(mac, frame_seq)`, the same key the
/// binary path uses, and nothing is lost for phase sync.
///
/// A line with no tail (an older build, mid-rollout) yields a zeroed `mac`, which degrades that
/// capture to arrival-time pairing rather than discarding it. Zeroed rather than invented: a
/// fabricated identity would let the join pair packets that were never the same transmission.
/// `aa:bb:cc:dd:ee:ff` → six bytes. `None` for anything else, including an empty tail.
fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = s.split(':');
    for slot in out.iter_mut() {
        *slot = u8::from_str_radix(parts.next()?.trim(), 16).ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

pub fn decode_array_list(line: &str) -> Option<DecodedCsi> {
    let line = line.trim();
    let body = line.strip_prefix('[')?.strip_suffix(']')?;
    // The payload array is the tail; the header is everything before it.
    let open = body.rfind('[')?;
    let header = body[..open].trim_end_matches(',');
    // The payload array, then whatever identity tail follows it.
    //
    // The firmware appends `,<mac>` after the payload's `]` so the header's per-chip field positions
    // stay put — see `format_array_list_into`. Split at the payload's own close rather than the end
    // of the line, and treat a missing tail as a line from an older build rather than a malformed
    // one: a fleet runs mixed firmware during a rollout, and refusing those would drop real captures.
    let rest = &body[open + 1..];
    let close = rest.find(']')?;
    let payload = &rest[..close];
    let tail = rest[close + 1..].trim_start_matches(',').trim();

    let nums: Vec<i64> = header
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(|f| f.parse::<i64>().ok())
        .collect::<Option<_>>()?;
    // Eight common fields plus the trailing `sig_len,csi_data_len` pair is the shortest a line can
    // legitimately be; anything shorter is a truncated line, not a layout this does not know.
    if nums.len() < 10 {
        return None;
    }

    let csi_data: Vec<i8> = if payload.trim().is_empty() {
        Vec::new()
    } else {
        payload
            .split(',')
            .map(|v| v.trim().parse::<i8>().ok())
            .collect::<Option<_>>()?
    };
    // The firmware's own count, and it must agree with what arrived. A line cut short by a serial
    // read that raced the writer would otherwise become a short frame that looks complete.
    let claimed = *nums.last()? as usize;
    if claimed != csi_data.len() {
        return None;
    }

    Some(DecodedCsi {
        mac: parse_mac(tail).unwrap_or([0u8; 6]),
        sequence_number: nums[0] as u16,
        rssi: nums[1] as i32,
        rate: nums[2] as u32,
        noise_floor: nums[3] as i32,
        channel: nums[4] as u32,
        timestamp: nums[5] as u32,
        sig_len: nums[6] as u32,
        rx_state: nums[7] as u32,
        csi_data_len: claimed as u16,
        csi_data,
        // Everything else the line does not carry stays absent rather than defaulted to a value that
        // would read as a measurement.
        ..Default::default()
    })
}

/// Failure decoding a serialized CSI frame.
#[derive(Debug)]
pub enum DecodeError {
    /// A wire-format frame from another format version. The envelope decoded, so the version is
    /// known; the body was not attempted.
    UnsupportedVersion(u8),
    /// A valid frame that carries no CSI (a session announcement), passed to [`decode`].
    NotCsi,
    /// Neither the wire format nor the chip's pre-0.12 layout.
    Malformed(postcard::Error),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion(v) => write!(f, "unsupported CSI wire format version {v}"),
            Self::NotCsi => write!(f, "frame carries no CSI"),
            Self::Malformed(e) => write!(f, "failed to decode CSI frame: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<postcard::Error> for DecodeError {
    fn from(e: postcard::Error) -> Self {
        DecodeError::Malformed(e)
    }
}

/// A decoded serialized frame.
// Moved once per frame from decoder to sink; boxing the measurement would add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum DecodedFrame {
    /// One measurement.
    Csi(DecodedCsi),
    /// The session announcement a 0.12+ node emits at the start of each run.
    Session { envelope: Envelope, info: SessionInfo },
}

/// Decode one COBS frame (up to, not including, the `\0` terminator).
///
/// Tries the wire format first, then the pre-0.12 layout for `chip`, so firmware from before the
/// format change keeps decoding. A frame that carries another wire version and is not a valid
/// pre-0.12 frame either is [`DecodeError::UnsupportedVersion`].
pub fn decode_frame(frame: &[u8], chip: ChipVariant) -> Result<DecodedFrame, DecodeError> {
    let mut owned = frame.to_vec();
    match wire::decode_cobs(&mut owned) {
        Ok((envelope, Body::Csi(f))) => return Ok(DecodedFrame::Csi(from_wire(&envelope, f))),
        Ok((envelope, Body::Session(info))) => return Ok(DecodedFrame::Session { envelope, info }),
        // A pre-0.12 frame starts with a MAC address, whose first byte reads as a version: only
        // report a foreign version if the legacy layout does not decode either.
        Err(wire::DecodeError::UnsupportedVersion(v)) => {
            return decode_legacy(frame, chip)
                .map(DecodedFrame::Csi)
                .map_err(|_| DecodeError::UnsupportedVersion(v));
        }
        Err(_) => {}
    }
    decode_legacy(frame, chip).map(DecodedFrame::Csi)
}

/// Decode one frame that must carry CSI. See [`decode_frame`]; a session announcement is
/// [`DecodeError::NotCsi`].
pub fn decode(frame: &[u8], chip: ChipVariant) -> Result<DecodedCsi, DecodeError> {
    match decode_frame(frame, chip)? {
        DecodedFrame::Csi(c) => Ok(c),
        DecodedFrame::Session { .. } => Err(DecodeError::NotCsi),
    }
}

/// Decode a pre-0.12 `CSIDataPacket` frame in `chip`'s layout.
pub fn decode_legacy(frame: &[u8], chip: ChipVariant) -> Result<DecodedCsi, DecodeError> {
    let mut owned = frame.to_vec();
    let decoded = match chip {
        ChipVariant::Esp32Family => {
            let (p, _) = postcard::take_from_bytes_cobs::<PacketA>(&mut owned)?;
            p.into()
        }
        ChipVariant::Esp32c5 => {
            let (p, _) = postcard::take_from_bytes_cobs::<PacketBc5>(&mut owned)?;
            p.into()
        }
        ChipVariant::Esp32c6 => {
            let (p, _) = postcard::take_from_bytes_cobs::<PacketBc6>(&mut owned)?;
            p.into()
        }
    };
    Ok(decoded)
}

/// The pre-0.12 compact format code, rebuilt from normalised metadata so consumers that key on
/// [`RxCsiFmt`] keep working. Formats it never named (HE) stay `Undefined`, with the raw
/// `cur_bb_format` preserved alongside.
fn legacy_format(m: &wire::RxMeta) -> RxCsiFmt {
    use wire::{Bandwidth, PpduFormat, Secondary};
    let forty = m.bandwidth == Some(Bandwidth::Mhz40);
    let stbc = m.stbc == Some(true);
    match (m.secondary, m.ppdu) {
        (Secondary::None, PpduFormat::NonHt | PpduFormat::Dsss) => RxCsiFmt::Bw20,
        (Secondary::None, PpduFormat::Ht) if stbc => RxCsiFmt::HtBw20Stbc,
        (Secondary::None, PpduFormat::Ht) => RxCsiFmt::HtBw20,
        (Secondary::Below, PpduFormat::NonHt | PpduFormat::Dsss) => RxCsiFmt::SecbBw20,
        (Secondary::Below, PpduFormat::Ht) => match (forty, stbc) {
            (false, false) => RxCsiFmt::SecbHtBw20,
            (false, true) => RxCsiFmt::SecbHtBw20Stbc,
            (true, false) => RxCsiFmt::SecbHtBw40,
            (true, true) => RxCsiFmt::SecbHtBw40Stbc,
        },
        (Secondary::Above, PpduFormat::NonHt | PpduFormat::Dsss) => RxCsiFmt::SecaBw20,
        (Secondary::Above, PpduFormat::Ht) => match (forty, stbc) {
            (false, false) => RxCsiFmt::SecaHtBw20,
            (false, true) => RxCsiFmt::SecaHtBw20Stbc,
            (true, false) => RxCsiFmt::SecaHtBw40,
            (true, true) => RxCsiFmt::SecaHtBw40Stbc,
        },
        (_, PpduFormat::Vht) => RxCsiFmt::VhtBw20,
        _ => RxCsiFmt::Undefined,
    }
}

/// Flatten a wire-format measurement into the superset record.
pub fn from_wire(env: &Envelope, f: wire::CsiFrame) -> DecodedCsi {
    let m = &f.meta;
    let secondary = match m.secondary {
        wire::Secondary::Above => 1,
        wire::Secondary::Below => 2,
        _ => 0,
    };
    let mut d = DecodedCsi {
        mac: f.transmitter().unwrap_or([0; 6]),
        rssi: m.rssi as i32,
        timestamp: m.timestamp_us as u32,
        noise_floor: m.noise_floor as i32,
        sig_len: m.sig_len as u32,
        rx_state: m.rx_state as u32,
        channel: m.channel as u32,
        sequence_number: m.frame_seq.unwrap_or(0),
        data_format: legacy_format(m),
        wire_version: Some(env.version),
        node_id: Some(env.node_id),
        session_id: Some(env.session_id),
        stream_seq: Some(env.stream_seq),
        source: Some(env.source),
        timestamp_us: Some(m.timestamp_us),
        ppdu: Some(m.ppdu),
        bandwidth_mhz: m.bandwidth.map(|b| b.mhz()),
        n_rx: Some(m.n_rx),
        n_ss: m.n_ss,
        stimulus: Some(f.stimulus),
        header: f.header,
        sgi: m.sgi.map(u32::from),
        stbc: m.stbc.map(u32::from),
        mcs: m.mcs.map(u32::from),
        antenna: m.antenna.map(u32::from),
        not_sounding: m.not_sounding.map(u32::from),
        aggregation: m.aggregation.map(u32::from),
        ..Default::default()
    };
    match m.vendor {
        VendorRx::EspClassic { rate, sig_mode, smoothing, fec_ldpc, ampdu_cnt } => {
            d.rate = rate as u32;
            d.sig_mode = Some(sig_mode as u32);
            d.smoothing = Some(smoothing as u32);
            d.fec_coding = Some(fec_ldpc as u32);
            d.ampdu_cnt = Some(ampdu_cnt as u32);
            d.secondary_channel = Some(secondary);
            d.bandwidth = Some((m.bandwidth == Some(wire::Bandwidth::Mhz40)) as u32);
        }
        VendorRx::EspHe {
            rate,
            cur_bb_format,
            estimate_valid,
            estimate_len,
            dump_len,
            is_group,
            rxend_state,
            rxmatch,
            sigb_len,
            single_mpdu,
            ..
        } => {
            d.rate = rate as u32;
            d.cur_bb_format = Some(cur_bb_format as u32);
            d.rx_channel_estimate_info_vld = Some(estimate_valid as u32);
            d.rx_channel_estimate_len = Some(estimate_len as u32);
            d.dump_len = Some(dump_len as u32);
            d.second = Some(secondary);
            d.is_group = Some(is_group as u32);
            d.rxend_state = Some(rxend_state as u32);
            d.rxmatch3 = Some(((rxmatch >> 3) & 1) as u32);
            d.rxmatch2 = Some(((rxmatch >> 2) & 1) as u32);
            d.rxmatch1 = Some(((rxmatch >> 1) & 1) as u32);
            d.rxmatch0 = Some((rxmatch & 1) as u32);
            d.sigb_len = Some(sigb_len as u32);
            d.cur_single_mpdu = Some(single_mpdu as u32);
        }
        _ => {}
    }
    match f.payload {
        CsiPayload::EspRaw { layout, first_word_invalid, bytes, .. } => {
            d.layout = Some(layout);
            d.first_word_invalid = Some(first_word_invalid);
            d.csi_data_len = bytes.len() as u16;
            d.csi_data = bytes.to_vec();
        }
        CsiPayload::Grouped { ng, nb, sc_start, n_sc, n_rx, n_tx, data } => {
            d.grouped = Some(GroupedReport { ng, nb, sc_start, n_sc, n_rx, n_tx, data: data.to_vec() });
        }
        CsiPayload::Variation { value } => d.variation = Some(value),
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a `PacketA` through postcard+COBS exactly as the firmware
    /// emits it (`to_slice_cobs`), then decode it back. Guards against wire
    /// drift in field order/types for the esp32-family layout.
    #[test]
    fn decode_packet_a_roundtrip() {
        let pkt = PacketA {
            mac: [0xde, 0xad, 0xbe, 0xef, 0x00, 0x01],
            rssi: -42,
            timestamp: 123_456,
            rate: 11,
            sgi: 1,
            secondary_channel: 0,
            channel: 6,
            bandwidth: 0,
            antenna: 0,
            sig_mode: 1,
            mcs: 7,
            smoothing: 0,
            not_sounding: 1,
            aggregation: 0,
            stbc: 0,
            fec_coding: 0,
            ampdu_cnt: 0,
            noise_floor: -96,
            rx_state: 0,
            sig_len: 128,
            date_time: Some(DateTime {
                year: 2026,
                month: 6,
                day: 22,
                hour: 12,
                minute: 30,
                second: 15,
                millisecond: 250,
            }),
            sequence_number: 4242,
            data_format: RxCsiFmt::HtBw20,
            csi_data_len: 4,
            csi_data: vec![1, -2, 3, -4],
        };

        // Mirror the firmware: postcard-serialize then COBS-frame.
        let mut buf = vec![0u8; 1024];
        let cobs = postcard::to_slice_cobs(&pkt, &mut buf).unwrap();
        // The serial reader strips the trailing `\0` terminator; mimic that.
        let body = cobs.strip_suffix(&[0]).unwrap_or(cobs);

        let out = decode(body, ChipVariant::Esp32Family).unwrap();
        assert_eq!(out.mac, pkt.mac);
        assert_eq!(out.rssi, -42);
        assert_eq!(out.channel, 6);
        assert_eq!(out.mcs, Some(7));
        assert_eq!(out.noise_floor, -96);
        assert_eq!(out.sequence_number, 4242);
        assert_eq!(out.data_format, RxCsiFmt::HtBw20);
        assert_eq!(out.csi_data, vec![1, -2, 3, -4]);
        assert_eq!(out.dump_len, None);
        let dt = out.date_time.expect("date_time present");
        assert_eq!((dt.year, dt.month, dt.day), (2026, 6, 22));
    }

    #[test]
    fn decode_packet_bc6_roundtrip() {
        let pkt = PacketBc6 {
            mac: [1, 2, 3, 4, 5, 6],
            rssi: -55,
            timestamp: 9,
            rate: 1,
            noise_floor: -90,
            sig_len: 64,
            rx_state: 0,
            dump_len: 100,
            sigb_len: 7,
            cur_single_mpdu: 1,
            cur_bb_format: 2,
            rx_channel_estimate_info_vld: 1,
            rx_channel_estimate_len: 64,
            second: 3,
            channel: 11,
            is_group: 0,
            rxend_state: 0,
            rxmatch3: 0,
            rxmatch2: 0,
            rxmatch1: 1,
            rxmatch0: 1,
            date_time: None,
            sequence_number: 7,
            csi_data_len: 2,
            data_format: RxCsiFmt::Undefined,
            csi_data: vec![-1, 1],
        };
        let mut buf = vec![0u8; 1024];
        let cobs = postcard::to_slice_cobs(&pkt, &mut buf).unwrap();
        let body = cobs.strip_suffix(&[0]).unwrap_or(cobs);

        let out = decode(body, ChipVariant::Esp32c6).unwrap();
        assert_eq!(out.mac, [1, 2, 3, 4, 5, 6]);
        assert_eq!(out.sigb_len, Some(7));
        assert_eq!(out.rxmatch0, Some(1));
        assert_eq!(out.sgi, None);
        assert_eq!(out.csi_data, vec![-1, 1]);
        assert!(out.date_time.is_none());
    }

    /// A clean esp32c5 frame round-trips through `to_slice_cobs` (as the
    /// firmware emits it) and back through `decode`.
    #[test]
    fn decode_packet_bc5_roundtrip() {
        let pkt = PacketBc5 {
            mac: [0xde, 0xad, 0xbe, 0xef, 0x00, 0x05],
            rssi: -61, timestamp: 123456, rate: 1, noise_floor: -92, sig_len: 80,
            rx_state: 0, dump_len: 384, cur_bb_format: 2, rx_channel_estimate_info_vld: 1,
            rx_channel_estimate_len: 384, second: 7, channel: 149, is_group: 0,
            rxend_state: 0, rxmatch3: 0, rxmatch2: 0, rxmatch1: 1, date_time: None,
            sequence_number: 99, csi_data_len: 4, data_format: RxCsiFmt::Undefined,
            csi_data: vec![1, -2, 3, -4],
        };
        let mut buf = vec![0u8; 2048];
        let cobs = postcard::to_slice_cobs(&pkt, &mut buf).unwrap();
        let body = cobs.strip_suffix(&[0]).unwrap_or(cobs);
        let out = decode(body, ChipVariant::Esp32c5).unwrap();
        assert_eq!(out.channel, 149);
        assert_eq!(out.mac, [0xde, 0xad, 0xbe, 0xef, 0x00, 0x05]);
        assert_eq!(out.csi_data, vec![1, -2, 3, -4]);
        assert_eq!(out.dump_len, Some(384));
        assert_eq!(out.sgi, None);
    }

    #[test]
    fn chip_string_mapping() {
        assert_eq!(ChipVariant::from_chip_str("ESP32"), Some(ChipVariant::Esp32Family));
        assert_eq!(ChipVariant::from_chip_str("esp32c6"), Some(ChipVariant::Esp32c6));
        assert_eq!(ChipVariant::from_chip_str("esp32c5"), Some(ChipVariant::Esp32c5));
        assert_eq!(ChipVariant::from_chip_str("weird"), None);
    }
}

#[cfg(test)]
mod array_list_tests {
    use super::*;

    /// A real C5 line, parsed from both ends rather than by a per-chip position table.
    #[test]
    fn a_c5_array_list_line_yields_the_fields_it_carries() {
        // seq,rssi,rate,noise_floor,channel,timestamp,sig_len,rx_state, <c5 diagnostics…>,
        // sig_len,csi_data_len,[payload]
        let line = "[7,-42,11,-96,149,123456,120,0,384,4,1,384,9,149,120,6,[1,-2,3,-4,5,-6]]\r\n";
        let d = decode_array_list(line).expect("a well-formed line");
        assert_eq!(d.sequence_number, 7);
        assert_eq!(d.rssi, -42);
        assert_eq!(d.rate, 11);
        assert_eq!(d.noise_floor, -96);
        assert_eq!(d.channel, 149);
        assert_eq!(d.timestamp, 123456);
        assert_eq!(d.csi_data_len, 6);
        assert_eq!(d.csi_data, vec![1, -2, 3, -4, 5, -6]);
        // No tail on this line, so identity degrades rather than being invented.
        assert_eq!(d.mac, [0u8; 6]);
    }

    /// A longer chip layout parses with no change, because nothing indexes the middle.
    #[test]
    fn a_wider_layout_parses_without_a_per_chip_table() {
        let line = "[1,-50,7,-95,6,99,64,0,1,2,3,4,5,6,7,8,9,10,64,4,[9,8,7,6]]";
        let d = decode_array_list(line).expect("a wider header is still readable");
        assert_eq!((d.sequence_number, d.channel, d.csi_data_len), (1, 6, 4));
        assert_eq!(d.csi_data, vec![9, 8, 7, 6]);
    }

    /// A line cut short by a read that raced the writer is rejected, not silently shortened.
    ///
    /// This is the failure mode that matters: the pool's serial read is not cancellation-safe and
    /// splices partial frames (the COBS path logs exactly that). A truncated payload that still
    /// parsed would become a short frame indistinguishable from a complete one.
    #[test]
    fn a_truncated_payload_is_refused_because_the_count_disagrees() {
        let full = "[7,-42,11,-96,149,123456,120,0,384,4,1,384,9,149,120,6,[1,-2,3,-4,5,-6]]";
        assert!(decode_array_list(full).is_some());
        let cut = "[7,-42,11,-96,149,123456,120,0,384,4,1,384,9,149,120,6,[1,-2,3]]";
        assert!(
            decode_array_list(cut).is_none(),
            "the firmware's own csi_data_len must agree with what arrived"
        );
    }

    /// Not-a-line, and a header too short to be any layout, are refused.
    #[test]
    fn malformed_input_is_refused() {
        assert!(decode_array_list("").is_none());
        assert!(decode_array_list("stats rx=49").is_none());
        assert!(decode_array_list("[1,2,3,[1,2]]").is_none(), "header too short");
        assert!(decode_array_list("[1,2,3,4,5,6,7,8,9,2,[1,x]]").is_none(), "non-numeric payload");
        // An empty payload with a matching count is legitimate: a metadata-only packet.
        assert!(decode_array_list("[1,2,3,4,5,6,7,8,9,0,[]]").is_some());
    }

    /// The identity tail is read, so an array-list capture joins on `(mac, frame_seq)` like the
    /// binary path — nothing is lost for phase sync.
    #[test]
    fn the_identity_tail_carries_mac_and_frame_seq_survives_as_field_zero() {
        let line = "[7,-42,11,-96,149,123456,120,0,384,4,1,384,9,149,120,6,\
                    [1,-2,3,-4,5,-6],10:bd:a3:ce:e5:b0]";
        let d = decode_array_list(line).expect("a line with an identity tail");
        assert_eq!(d.mac, [0x10, 0xbd, 0xa3, 0xce, 0xe5, 0xb0]);
        // `frame_seq` is this field: the pool maps `frame_seq: Some(decoded.sequence_number)`.
        assert_eq!(d.sequence_number, 7);
        assert_eq!(d.csi_data, vec![1, -2, 3, -4, 5, -6]);
    }

    /// A line from a build that predates the tail still parses — a fleet runs mixed firmware during
    /// a rollout, and refusing those would drop real captures.
    #[test]
    fn a_line_without_a_tail_still_parses_with_a_zeroed_mac() {
        let line = "[7,-42,11,-96,149,123456,120,0,384,4,1,384,9,149,120,6,[1,-2,3,-4,5,-6]]";
        let d = decode_array_list(line).expect("an older line is still readable");
        assert_eq!(d.mac, [0u8; 6]);
        assert_eq!(d.sequence_number, 7);
    }

    /// A malformed tail is not a fabricated identity.
    #[test]
    fn a_bad_mac_tail_degrades_rather_than_inventing_one() {
        for tail in ["zz:bd:a3:ce:e5:b0", "10:bd:a3", "10:bd:a3:ce:e5:b0:99"] {
            let line = format!(
                "[7,-42,11,-96,149,123456,120,0,384,4,1,384,9,149,120,2,[1,-2],{tail}]"
            );
            let d = decode_array_list(&line).expect("the frame itself is still good");
            assert_eq!(d.mac, [0u8; 6], "tail {tail:?} must not become an identity");
        }
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use crate::wire::{
        Bandwidth, Chip, CsiFrame, HeaderDigest, LayoutId, MAX_ENCODED_LEN, PpduFormat, RxMeta,
        Secondary, SourceKind,
    };

    fn he_frame() -> CsiFrame {
        let mut bytes = heapless::Vec::<i8, { wire::MAX_CSI_BYTES }>::new();
        for i in 0..490 {
            bytes.push((i % 50) as i8).unwrap();
        }
        CsiFrame::new(
            RxMeta {
                timestamp_us: 5_000_000_000,
                rssi: -40,
                noise_floor: -95,
                channel: 36,
                secondary: Secondary::None,
                bandwidth: Some(Bandwidth::Mhz20),
                ppdu: PpduFormat::HeSu,
                mcs: None,
                stbc: None,
                sgi: None,
                n_rx: 1,
                n_ss: None,
                antenna: None,
                sig_len: 100,
                rx_state: 0,
                not_sounding: None,
                aggregation: None,
                frame_seq: Some(77),
                vendor: VendorRx::EspHe {
                    rate: 16,
                    cur_bb_format: 4,
                    estimate_valid: true,
                    estimate_len: 490,
                    dump_len: 0,
                    is_group: false,
                    rxend_state: 0,
                    rxmatch: 0b1010,
                    he_siga1: 0,
                    he_siga2: 0,
                    sigb_len: 0,
                    single_mpdu: false,
                },
            },
            Stimulus::Controlled { setup_id: 5, instance_id: 9, ta: [1, 2, 3, 4, 5, 6] },
            Some(HeaderDigest {
                frame_control: 0x00d0,
                addr1: [0xff; 6],
                addr2: [1, 2, 3, 4, 5, 6],
                addr3: [7; 6],
                seq_ctrl: 77 << 4,
            }),
            CsiPayload::EspRaw {
                chip: Chip::Esp32C5,
                layout: LayoutId::C5He20Su,
                first_word_invalid: false,
                bytes,
            },
        )
    }

    fn encode(env: &Envelope, body: &Body) -> Vec<u8> {
        let mut buf = [0u8; MAX_ENCODED_LEN];
        let n = wire::encode_cobs(env, body, &mut buf).unwrap().len();
        buf[..n - 1].to_vec() // the serial task strips the `\0` terminator
    }

    #[test]
    fn wire_frame_decodes_into_the_superset_record() {
        let env = Envelope::new([9; 6], 0xabcd, SourceKind::EspVendor, 42);
        let d = decode(&encode(&env, &Body::Csi(he_frame())), ChipVariant::Esp32c5).unwrap();
        assert_eq!(d.wire_version, Some(wire::WIRE_VERSION));
        assert_eq!((d.node_id, d.session_id, d.stream_seq), (Some([9; 6]), Some(0xabcd), Some(42)));
        assert_eq!(d.timestamp_us, Some(5_000_000_000));
        assert_eq!(d.timestamp, 5_000_000_000u64 as u32);
        assert_eq!(d.mac, [1, 2, 3, 4, 5, 6]);
        assert_eq!(d.sequence_number, 77);
        assert_eq!(d.cur_bb_format, Some(4));
        assert_eq!(d.data_format, RxCsiFmt::Undefined);
        assert_eq!((d.rxmatch3, d.rxmatch1, d.rxmatch0), (Some(1), Some(1), Some(0)));
        assert_eq!(d.csi_data.len(), 490);
        assert_eq!(d.subcarrier_indices().unwrap().len(), 245);
        assert_eq!(d.subcarrier_freqs_hz().unwrap()[1], 78_125);
        assert!(matches!(d.stimulus, Some(Stimulus::Controlled { setup_id: 5, instance_id: 9, .. })));
    }

    #[test]
    fn session_announcement_is_a_session_frame() {
        let env = Envelope::new([9; 6], 7, SourceKind::EspVendor, 0);
        let info = SessionInfo::new(Chip::Esp32C5, [0, 12, 0], 1, Some(2));
        match decode_frame(&encode(&env, &Body::Session(info)), ChipVariant::Esp32c5).unwrap() {
            DecodedFrame::Session { envelope, info: got } => {
                assert_eq!(envelope.session_id, 7);
                assert_eq!(got, info);
            }
            other => panic!("expected a session frame, got {other:?}"),
        }
        assert!(matches!(
            decode(&encode(&env, &Body::Session(info)), ChipVariant::Esp32c5),
            Err(DecodeError::NotCsi)
        ));
    }

    #[test]
    fn foreign_wire_version_is_reported() {
        let mut env = Envelope::new([9; 6], 7, SourceKind::EspVendor, 0);
        env.version = wire::WIRE_VERSION + 1;
        let frame = encode(&env, &Body::Csi(he_frame()));
        assert!(matches!(
            decode(&frame, ChipVariant::Esp32c5),
            Err(DecodeError::UnsupportedVersion(v)) if v == wire::WIRE_VERSION + 1
        ));
    }
}
