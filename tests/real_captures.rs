//! Decode raw serial captures recorded from esp-csi-rs 0.12 firmware on ESP32-C5 boards.

use csi_webserver_core::csi::{self, ChipVariant, DecodedFrame};
use csi_webserver_core::wire::{LayoutId, PpduFormat, Stimulus};

/// Decode every frame of a capture, skipping the firmware's framed log lines.
fn decode_all(bytes: &[u8]) -> (Vec<csi::DecodedCsi>, usize) {
    let mut csi = Vec::new();
    let mut sessions = 0;
    for frame in bytes.split(|&b| b == 0).filter(|f| !f.is_empty()) {
        if frame.iter().all(|&b| b == b'\r' || b == b'\n' || (0x20..0x7f).contains(&b)) {
            continue;
        }
        match csi::decode_frame(frame, ChipVariant::Esp32c5) {
            Ok(DecodedFrame::Csi(d)) => csi.push(d),
            Ok(DecodedFrame::Session { .. }) => sessions += 1,
            Err(e) => panic!("frame failed to decode: {e}"),
        }
    }
    (csi, sessions)
}

#[test]
fn he20_sniffer_capture() {
    let (frames, _) = decode_all(include_bytes!("fixtures/c5_he20_sniffer.bin"));
    assert!(frames.len() > 20);
    for d in &frames {
        assert_eq!(d.ppdu, Some(PpduFormat::HeSu));
        assert_eq!(d.layout, Some(LayoutId::C5He20Su));
        assert_eq!(d.subcarrier_indices().map(|v| v.len()), Some(245));
        assert!(d.header.is_some(), "a sniffer captures the MAC header digest");
    }
    // The capture opens with one frame from the previous boot, under another session id; the
    // counter is contiguous within each session.
    let pairs: Vec<(u32, u32)> =
        frames.iter().map(|d| (d.session_id.unwrap(), d.stream_seq.unwrap())).collect();
    assert!(
        pairs.windows(2).all(|w| w[0].0 != w[1].0 || w[1].1 == w[0].1 + 1),
        "stream_seq is contiguous within a session"
    );
}

#[test]
fn espnow_central_capture_carries_setup_and_instances() {
    let (frames, _) = decode_all(include_bytes!("fixtures/c5_espnow_central_setup5.bin"));
    assert!(frames.len() > 20);
    let instances: Vec<u16> = frames
        .iter()
        .map(|d| match d.stimulus {
            Some(Stimulus::Controlled { setup_id: 5, instance_id, .. }) => instance_id,
            other => panic!("expected setup 5, got {other:?}"),
        })
        .collect();
    assert!(instances.windows(2).all(|w| w[1] >= w[0]), "instances advance");
}

#[test]
fn threshold_variation_capture() {
    let (frames, _) = decode_all(include_bytes!("fixtures/c5_threshold_variation.bin"));
    assert!(frames.len() > 20);
    assert!(frames.iter().all(|d| d.variation.is_some() && d.csi_data.is_empty()));
}
