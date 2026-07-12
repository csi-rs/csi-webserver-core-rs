//! Serial connection lifecycle and frame forwarding.
//!
//! Each attached device gets a background task that reconnects automatically,
//! accepts CLI commands from route handlers, and splits the incoming stream
//! into COBS frames (the wire is always the firmware's `serialized` mode).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::{Duration, sleep};
use tokio_serial::{ClearBuffer, SerialPort, SerialPortBuilderExt};

use crate::csi::{self, ChipVariant};
use crate::models::{DeviceConfig, DeviceInfo, OutputMode};
use crate::parquet_sink::ParquetSink;
use crate::profile::CsiProfile;
use crate::state::{DeviceAttachSpec, DeviceHandle, InfoResponder};

/// Distinguishes "firmware-not-present / parse-failure" from "the link itself
/// died" so the caller can decide whether to surface a `Result` or to
/// reconnect.
#[derive(Debug)]
enum InfoExchangeError {
    /// Logical failure — magic prefix never seen, timed out, or parse error.
    /// Connection is still healthy.
    Soft(String),
    /// I/O failure — connection is broken; the outer loop should reconnect.
    Hard(String),
    /// The bytes received instead of an info block match a known chip fault
    /// signature (ROM boot loop, USB-JTAG reset wedge, download mode). The
    /// connection is healthy but the chip will not become responsive on its
    /// own; surfaced to clients via the device's `fault` field.
    BootFault(String),
}

impl InfoExchangeError {
    fn message(&self) -> &str {
        match self {
            Self::Soft(m) | Self::Hard(m) | Self::BootFault(m) => m,
        }
    }
}

/// Classify raw serial bytes against known chip fault signatures. Returns a
/// user-facing description (including the recovery action) or `None`.
///
/// A single ROM banner followed by CLI output is a normal boot; a *repeating*
/// banner during one info exchange window means the chip is reset-looping and
/// never reaches the CLI, and a lone `USB_UART_*` banner with nothing after it
/// means the chip halted right there (same wedge, caught mid-flight).
/// The `USB_UART_HPSYS` cause is the known ESP32-C5/C6 post-flash wedge
/// (esp-rs/espflash#556): software resets cannot recover it — only a USB
/// power cycle (replug, or `uhubctl -a cycle` on a switchable hub) does.
fn detect_boot_fault(rx: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(rx);
    let usb_wedge_resets =
        text.matches("USB_UART_HPSYS").count() + text.matches("USB_UART_CHIP_RESET").count();
    if usb_wedge_resets >= 2 {
        return Some(
            "USB-JTAG reset loop (rst:0x15 USB_UART_HPSYS) — known ESP32-C5/C6 post-flash \
             wedge; software reset cannot recover it. Power-cycle the USB port: replug the \
             board, or `uhubctl -a cycle` on a power-switchable hub."
                .to_string(),
        );
    }
    if usb_wedge_resets == 1 && !text.contains("ESP-CSI-CLI") {
        // Only reached when the info exchange already timed out, so the chip
        // reset via USB-JTAG and then never came back to the CLI. (A healthy
        // chip that was merely slow to boot clears this on the next re-verify
        // tick.)
        return Some(
            "chip halted after a USB-JTAG-triggered reset (rst:0x15 USB_UART_HPSYS) — known \
             ESP32-C5/C6 wedge; software reset cannot recover it. Power-cycle the USB port \
             (replug, or `uhubctl -a cycle`); if it persists across a power cycle, reflash \
             the firmware."
                .to_string(),
        );
    }
    if text.contains("waiting for download") {
        return Some(
            "chip is stuck in ROM download mode (waiting for download) — reflash it or \
             reset out of download mode"
                .to_string(),
        );
    }
    if text.matches("ESP-ROM:").count() >= 3 {
        return Some(
            "boot loop: ROM banner repeating; firmware never reaches the CLI — reflash a \
             known-good image or power-cycle the board"
                .to_string(),
        );
    }
    None
}

/// How long to wait for the device to emit a complete info block before
/// failing the request. The firmware prints the block synchronously in
/// response to `info`, so anything significantly above the round-trip time
/// signals that the firmware is missing or unresponsive.
const INFO_RESPONSE_TIMEOUT: Duration = Duration::from_millis(2000);

/// How often the connection loop re-attempts firmware verification while the
/// link is up but the device is still unverified (and not collecting). The
/// initial auto-verify on connect can miss — the chip may still be booting,
/// the RTS reset may not have landed on a native-USB board, or the device may
/// have been mid-stream when the server started. Without this retry, a device
/// attached before server start would sit `firmware_verified == false` until a
/// full reconnect, never becoming usable to clients.
const REVERIFY_INTERVAL: Duration = Duration::from_secs(3);

/// Per-device CSI frame broadcast buffer, in frames.
const CSI_BROADCAST_CAPACITY: usize = 1024;

/// Build a [`DeviceHandle`] for a port, wire up its channels, and spawn the
/// per-device serial task.
pub fn spawn_device(spec: &DeviceAttachSpec, profile: Arc<dyn CsiProfile>) -> Arc<DeviceHandle> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<String>(64);
    let (csi_tx, _) = broadcast::channel::<Vec<u8>>(CSI_BROADCAST_CAPACITY);
    let (output_mode_tx, output_mode_rx) = watch::channel(OutputMode::default());
    let (session_file_tx, session_file_rx) = watch::channel::<Option<String>>(None);
    let (info_request_tx, info_request_rx) = mpsc::channel::<InfoResponder>(4);

    let dev = Arc::new(DeviceHandle {
        id: spec.id.clone(),
        mac: spec.mac.clone(),
        port_path: spec.port_path.clone(),
        baud_rate: spec.baud_rate,
        native_usb: spec.native_usb,
        serial_connected: AtomicBool::new(false),
        collection_running: AtomicBool::new(false),
        firmware_verified: AtomicBool::new(false),
        cmd_tx,
        csi_tx,
        output_mode_tx,
        session_file_tx,
        info_request_tx,
        config: tokio::sync::Mutex::new(DeviceConfig::default()),
        device_info: tokio::sync::Mutex::new(None),
        fault: tokio::sync::Mutex::new(spec.fault.clone()),
        recovery_cycles: std::sync::atomic::AtomicU32::new(spec.recovery_cycles),
        shutdown: tokio_util::sync::CancellationToken::new(),
        profile,
    });

    tokio::spawn(run_serial_task(
        dev.clone(),
        cmd_rx,
        output_mode_rx,
        session_file_rx,
        info_request_rx,
    ));

    dev
}

/// Background task: owns the serial port for its lifetime.
///
/// - Continuously reconnects if the ESP32 disconnects.
/// - Reads incoming CSI frames from the serial port. The wire format is always
///   the firmware's `serialized` mode: COBS-framed postcard records delimited
///   by `\0`. Each frame is broadcast verbatim to WebSocket subscribers via
///   `csi_tx` and, when dumping, decoded and written to a Parquet session file.
/// - Watches `cmd_rx` for outgoing CLI command strings and writes them to the
///   port, appending a newline.
pub async fn run_serial_task(
    dev: Arc<DeviceHandle>,
    mut cmd_rx: mpsc::Receiver<String>,
    mut output_mode_rx: watch::Receiver<OutputMode>,
    mut session_file_rx: watch::Receiver<Option<String>>,
    mut info_request_rx: mpsc::Receiver<InfoResponder>,
) {
    let port_path = dev.port_path.clone();
    let baud = dev.baud_rate;
    const RECONNECT_DELAY: Duration = Duration::from_millis(800);

    loop {
        if dev.shutdown.is_cancelled() {
            break;
        }

        let mut stream = match tokio_serial::new(&port_path, baud).open_native_async() {
            Ok(s) => s,
            Err(e) => {
                dev.serial_connected.store(false, Ordering::SeqCst);
                dev.collection_running.store(false, Ordering::SeqCst);
                tracing::warn!("Failed to open serial port {port_path}: {e}. Retrying...");
                tokio::select! {
                    _ = sleep(RECONNECT_DELAY) => continue,
                    _ = dev.shutdown.cancelled() => break,
                }
            }
        };

        #[cfg(unix)]
        {
            // Allow opening a short-lived second handle for RTS reset operations.
            let _ = stream.set_exclusive(false);
        }

        // Auto-reset the ESP32 right after a successful serial connection by
        // pulsing RTS (RTS→EN). This matches the devkit EN/RTS wiring used by
        // ESP32 USB-UART boards (CP210x / CH340) and is what gets them to
        // (re)initialise and start answering `info` after the port opens.
        //
        // Skipped for native USB-Serial-JTAG chips (VID 0x303A). On those the
        // RTS/DTR pulse reboots the USB peripheral itself, so the device
        // re-enumerates and its `/dev/ttyACMx` node can return under a
        // different number — or, on slower hosts like the Raspberry Pi, the
        // re-enumeration races with the pinned-path reconnect and leaves the
        // USB CDC endpoint wedged (writes time out, the board never verifies,
        // and only a physical replug recovers it). These chips already answer
        // `info` without a reset; `quiesce_stale_stream` (a `q` to stop any
        // auto-stream) plus the periodic re-verify loop wakes them instead.
        if dev.native_usb {
            // No RTS auto-reset (it would re-enumerate the port), but DO
            // release the modem-control lines. On USB-Serial-JTAG, DTR/RTS
            // emulate the reset/boot straps: a previous tool (espflash, a
            // terminal, or the OS open itself) can leave them latched in an
            // active combination, and the chip then resets on every
            // interaction — verification never succeeds until the latch is
            // cleared.
            //
            // ORDER MATTERS. The kernel raises DTR and RTS together at open
            // (one SET_CONTROL_LINE_STATE); releasing DTR first transits
            // through {DTR=0, RTS=1}, which the peripheral maps to EN low —
            // esptool's HardReset assert state — so dropping RTS afterwards
            // completes a warm chip reset (rst:0x15 USB_UART_HPSYS) on EVERY
            // open. Healthy chips reboot through it (that was the mystery
            // "stale stream" flood on connect); wedge-prone C5s halt in ROM
            // instead and need a power cycle. Releasing RTS first transits
            // through {DTR=1, RTS=0} (BOOT latch, harmless without a reset
            // edge) and never asserts EN: the running app is left untouched.
            let _ = stream.write_request_to_send(false);
            let _ = stream.write_data_terminal_ready(false);
            tracing::info!(
                "Skipping RTS auto-reset on {port_path} (native USB-Serial-JTAG; reset would re-enumerate the port); RTS/DTR released (RTS first — DTR-first warm-resets the chip)"
            );
        } else {
            let _ = stream.write_data_terminal_ready(false);
            if let Err(e) = stream.write_request_to_send(true) {
                tracing::warn!("Failed to assert RTS on {port_path}: {e}");
            } else {
                sleep(Duration::from_millis(100)).await;
                if let Err(e) = stream.write_request_to_send(false) {
                    tracing::warn!("Failed to deassert RTS on {port_path}: {e}");
                } else {
                    tracing::info!("ESP32 reset on connect via RTS ({port_path})");
                }
            }
        }

        // Dump whatever the kernel buffered before this open (bootloader
        // banners, a previous session's CSI flood, half-written frames) so the
        // reader starts from a clean slate. `quiesce_stale_stream` still
        // handles bytes the device keeps emitting *after* this point.
        if let Err(e) = stream.clear(ClearBuffer::All) {
            tracing::warn!("Failed to clear stale port buffers on {port_path}: {e}");
        }

        dev.serial_connected.store(true, Ordering::SeqCst);
        tracing::info!("Opened serial port {port_path} @ {baud} baud");

        let exit = run_serial_connection(
            &dev,
            stream,
            &mut cmd_rx,
            &mut output_mode_rx,
            &mut session_file_rx,
            &mut info_request_rx,
        )
        .await;

        dev.serial_connected.store(false, Ordering::SeqCst);
        dev.collection_running.store(false, Ordering::SeqCst);
        // Disconnect invalidates the firmware identity — a different chip
        // may be re-attached on reconnect, so force a fresh verification.
        dev.firmware_verified.store(false, Ordering::SeqCst);
        *dev.device_info.lock().await = None;

        match exit {
            ConnectionExit::Disconnected => {
                // Pinned to dev.port_path — retry the SAME port, never re-detect
                // (re-detecting would let two device tasks race for one port).
                tracing::warn!("ESP32 disconnected on {port_path}; waiting for reconnect...");
                tokio::select! {
                    _ = sleep(RECONNECT_DELAY) => {}
                    _ = dev.shutdown.cancelled() => break,
                }
            }
            ConnectionExit::CommandChannelClosed => {
                tracing::info!("Command channel closed — shutting down serial task ({port_path})");
                break;
            }
            ConnectionExit::Shutdown => {
                tracing::info!("Device unplugged — shutting down serial task ({port_path})");
                break;
            }
        }
    }
}

enum ConnectionExit {
    Disconnected,
    CommandChannelClosed,
    Shutdown,
}

/// The connected chip's identity and its CSI wire layout, derived from the
/// firmware `info` block. Only constructible for chips with a known layout.
struct ChipInfo {
    /// Chip string as reported by the firmware (e.g. `esp32c6`).
    name: String,
    /// Wire layout the Parquet decoder applies.
    variant: ChipVariant,
}

impl ChipInfo {
    fn from_info(info: &DeviceInfo) -> Option<Self> {
        let name = info.chip.clone()?;
        let variant = ChipVariant::from_chip_str(&name)?;
        Some(ChipInfo { name, variant })
    }
}

async fn run_serial_connection(
    dev: &DeviceHandle,
    stream: tokio_serial::SerialStream,
    cmd_rx: &mut mpsc::Receiver<String>,
    output_mode_rx: &mut watch::Receiver<OutputMode>,
    session_file_rx: &mut watch::Receiver<Option<String>>,
    info_request_rx: &mut mpsc::Receiver<InfoResponder>,
) -> ConnectionExit {
    let port_path = dev.port_path.as_str();
    let csi_tx = &dev.csi_tx;
    let collection_running = &dev.collection_running;
    let firmware_verified = &dev.firmware_verified;
    let device_info = &dev.device_info;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();

    // ── Auto-verify firmware on connect ───────────────────────────────────
    // The chip just rebooted via the RTS pulse in run_serial_task. Give it
    // a moment to finish printing its boot banner, then ask `info` and
    // mirror the result into AppState. This is what makes command
    // endpoints unblock without requiring the user to call /api/info first.

    // The chip identity (from the `info` block) selects the wire layout the
    // Parquet decoder uses; refreshed on every successful info exchange.
    let mut chip: Option<ChipInfo> = None;

    sleep(Duration::from_millis(700)).await;
    // Native USB-Serial-JTAG chips skip the RTS reset above, so a device left
    // in a stale collecting state (e.g. an auto-start firmware, or a previous
    // run) keeps flooding binary CSI and would bury the `info` request. Stop
    // and drain it first so the CLI is responsive before we verify.
    let boot_context = if dev.native_usb {
        quiesce_stale_stream(&mut writer, &mut reader, port_path).await
    } else {
        Vec::new()
    };
    match do_info_exchange(&mut writer, &mut reader, &boot_context).await {
        Ok(info) => {
            tracing::info!(
                "Firmware verified: esp-csi-cli-rs/{} ({})",
                info.banner_version,
                info.chip.as_deref().unwrap_or("unknown chip"),
            );
            chip = ChipInfo::from_info(&info);
            firmware_verified.store(true, Ordering::SeqCst);
            *device_info.lock().await = Some(info);
            *dev.fault.lock().await = None;
            dev.recovery_cycles.store(0, Ordering::SeqCst);
        }
        Err(e) => {
            tracing::warn!(
                "Firmware not verified on {port_path}: {}. Command endpoints will return 412 Precondition Failed until verification succeeds.",
                e.message(),
            );
            firmware_verified.store(false, Ordering::SeqCst);
            *device_info.lock().await = None;
            if let InfoExchangeError::BootFault(fault) = &e {
                tracing::error!("Chip fault detected on {port_path}: {fault}");
                *dev.fault.lock().await = Some(fault.clone());
            }
            if matches!(e, InfoExchangeError::Hard(_)) {
                return ConnectionExit::Disconnected;
            }
        }
    }

    // ── Output state (owned exclusively by this task) ─────────────────────
    // The wire is always serialized (COBS-framed postcard), so framing is
    // fixed; `drop_next_chunk` still skips the CLI echo straddling the first
    // COBS terminator after a command/transition.
    let mut current_mode = output_mode_rx.borrow().clone();
    let mut current_session_path = session_file_rx.borrow().clone();
    let mut drop_next_chunk = true;
    let mut sink: Option<ParquetSink> = None;
    let mut decode_errors: u64 = 0;

    // Open the Parquet sink immediately if mode/session already require it.
    sync_parquet_sink(&current_mode, &current_session_path, chip.as_ref(), &mut sink, &dev.profile);

    // Per-second throughput counters. Reported on `metrics` ticks to expose
    // where a stream stalls: `frames_in` is what we pull off the serial port,
    // `frames_broadcast` is what reaches the per-device broadcast channel. A
    // gap between them (or asymmetry between two devices) localises the
    // bottleneck — see the matching WebSocket-side metrics in `routes::ws`.
    let mut frames_in: u64 = 0;
    let mut frames_broadcast: u64 = 0;
    let mut metrics = tokio::time::interval(Duration::from_secs(1));
    metrics.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    metrics.tick().await; // consume the immediate first tick

    // Periodically re-attempt firmware verification if the initial auto-verify
    // above did not succeed. The first tick fires immediately, so consume it
    // here to avoid re-verifying on the very next loop iteration.
    let mut reverify = tokio::time::interval(REVERIFY_INTERVAL);
    reverify.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    reverify.tick().await;

    // Escalating recovery for a device that stays unverified (stale firmware
    // state the `q` quiesce can't clear). Counted per connection; each rung
    // fires once: soft failure #2 → firmware `restart` (CLI-level reboot),
    // #4 → USB-JTAG DTR/RTS reset pulse (native USB only; a UART bridge was
    // already hardware-reset on connect), #6 → terminal fault naming the
    // manual action. None of these can revive a firmware that panicked and
    // parked the CPU — that is fixed firmware-side (esp-backtrace
    // `custom-halt` → full-system watchdog reset in esp-csi-cli-rs ≥ 0.7.2).
    let mut verify_failures: u32 = 0;
    let mut restart_sent = false;
    // Snapshot the carried-over recovery count NOW: the ladder below bumps
    // `dev.recovery_cycles` itself (so a re-enumeration carries it into the
    // replacement handle), and re-reading it after our own bump would make
    // every rung past `restart` unreachable and fast-fault instead.
    let carried_cycles = dev.recovery_cycles.load(Ordering::SeqCst);
    let mut hw_reset_done = false;

    loop {
        // ── React to runtime output-mode or session-file changes ──────────
        let mode_changed = output_mode_rx.has_changed().unwrap_or(false);
        let session_changed = session_file_rx.has_changed().unwrap_or(false);

        if mode_changed {
            current_mode = output_mode_rx.borrow_and_update().clone();
        }
        if session_changed {
            match session_file_rx.borrow_and_update().clone() {
                Some(path) => current_session_path = Some(path),
                None => {
                    // Dropping the sink flushes remaining rows and writes the
                    // Parquet footer (see ParquetSink::Drop).
                    sink = None;
                    current_session_path = None;
                    tracing::info!("Session ended — Parquet file finalized");
                }
            }
        }
        if mode_changed || session_changed {
            sync_parquet_sink(&current_mode, &current_session_path, chip.as_ref(), &mut sink, &dev.profile);
        }

        // The wire is always serialized: COBS frames terminated by `\0`.
        const DELIMITER: u8 = b'\0';

        tokio::select! {
            // ── Per-second throughput report (only while collecting) ──────
            _ = metrics.tick() => {
                if collection_running.load(Ordering::SeqCst) {
                    tracing::debug!(
                        target: "csi_metrics",
                        "{port_path}: serial_in={frames_in}/s broadcast_out={frames_broadcast}/s ws_clients={}",
                        csi_tx.receiver_count(),
                    );
                }
                frames_in = 0;
                frames_broadcast = 0;
            }

            _ = dev.shutdown.cancelled() => {
                return ConnectionExit::Shutdown;
            }

            // ── Re-verify firmware while unverified and idle ──────────────
            // The branch is disabled once verified or while collecting (the
            // CLI is locked during collection), so a healthy device stops
            // probing as soon as it identifies itself.
            _ = reverify.tick(), if !firmware_verified.load(Ordering::SeqCst)
                && !collection_running.load(Ordering::SeqCst) =>
            {
                // The info block is text; drop the COBS chunk straddling it.
                drop_next_chunk = true;
                // Drop any partial frame; the info exchange runs in line-mode.
                buf.clear();
                // A native-USB device may still be flooding from a stale
                // session; stop and drain it before re-probing.
                let boot_context = if dev.native_usb {
                    quiesce_stale_stream(&mut writer, &mut reader, port_path).await
                } else {
                    Vec::new()
                };

                let mut verify_failed = false;
                match do_info_exchange(&mut writer, &mut reader, &boot_context).await {
                    Ok(info) => {
                        tracing::info!(
                            "Firmware verified on retry: esp-csi-cli-rs/{} ({})",
                            info.banner_version,
                            info.chip.as_deref().unwrap_or("unknown chip"),
                        );
                        chip = ChipInfo::from_info(&info);
                        firmware_verified.store(true, Ordering::SeqCst);
                        *device_info.lock().await = Some(info);
                        *dev.fault.lock().await = None;
                        dev.recovery_cycles.store(0, Ordering::SeqCst);
                        verify_failures = 0;
                    }
                    Err(InfoExchangeError::Soft(msg)) => {
                        // Still not esp-csi-cli-rs (or not responding yet);
                        // surface periodically so a stuck device is visible
                        // rather than silently retrying forever.
                        tracing::debug!("Re-verify on {port_path} still failing: {msg}");
                        verify_failed = true;
                    }
                    Err(InfoExchangeError::BootFault(fault)) => {
                        // Log loudly only on the first sighting; the re-verify
                        // tick would otherwise repeat this every few seconds.
                        let mut slot = dev.fault.lock().await;
                        if slot.as_deref() != Some(fault.as_str()) {
                            tracing::error!("Chip fault detected on {port_path}: {fault}");
                            *slot = Some(fault);
                        }
                        verify_failed = true;
                    }
                    Err(InfoExchangeError::Hard(msg)) => {
                        tracing::warn!("Serial link error during re-verify on {port_path}: {msg}");
                        return ConnectionExit::Disconnected;
                    }
                }

                // ── Escalating recovery for a persistently unverified device ──
                // Ladder on a fresh device: firmware `restart` (2 failures) →
                // USB-JTAG DTR/RTS pulse (4) → terminal fault (6). A restart
                // that DOES reboot the chip re-enumerates native USB and
                // replaces this handle — the carried-over `recovery_cycles`
                // makes the follow-up connection skip the rungs (they already
                // ran) and declare the fault promptly instead of looping.
                if verify_failed {
                    verify_failures += 1;
                    if carried_cycles >= 1 && verify_failures >= 2 {
                        // A previous connection already ran the ladder and the
                        // chip re-enumerated into this handle still broken —
                        // hand it to the human instead of looping the rungs.
                        declare_terminal_fault(dev, port_path).await;
                    } else if verify_failures >= 2 && !restart_sent {
                        // CLI-level reboot: works whenever the firmware's REPL
                        // is alive but its state is stale in a way `q` +
                        // `info` can't recover. The reboot re-enumerates
                        // native USB; reconnect + auto-verify finish the job.
                        restart_sent = true;
                        dev.recovery_cycles.fetch_add(1, Ordering::SeqCst);
                        tracing::warn!(
                            "Device on {port_path} unverified after {verify_failures} attempts — sending firmware `restart`"
                        );
                        // Bounded like the other verify-path writes: a wedged
                        // USB endpoint must not freeze this task.
                        let _ = tokio::time::timeout(
                            Duration::from_secs(2),
                            writer.write_all(b"\r\nrestart\r\n"),
                        )
                        .await;
                    } else if verify_failures >= 4 && dev.native_usb && !hw_reset_done {
                        // The REPL isn't reachable (restart had no effect):
                        // pulse the chip's reset via USB-JTAG modem lines.
                        // Best-effort — ESP32-C5 firmware at runtime has been
                        // observed to ignore DTR/RTS strap-emulation resets.
                        hw_reset_done = true;
                        tracing::warn!(
                            "Device on {port_path} still unverified — pulsing USB-JTAG DTR/RTS reset (best-effort)"
                        );
                        match usb_jtag_reset_pulse(port_path, dev.baud_rate).await {
                            Ok(()) => tracing::info!(
                                "USB-JTAG reset pulse sent on {port_path}; if the chip honors it, the port re-enumerates and re-verifies"
                            ),
                            Err(e) => tracing::warn!(
                                "USB-JTAG reset pulse failed on {port_path}: {e}"
                            ),
                        }
                    } else if verify_failures >= 6 && restart_sent {
                        // Everything the host can do over serial has run in
                        // this connection — hand it to the human instead of
                        // retrying forever.
                        declare_terminal_fault(dev, port_path).await;
                    }
                }
            }

            result = reader.read_until(DELIMITER, &mut buf) => {
                match result {
                    Ok(0) => {
                        tracing::warn!("Serial port {port_path} closed (EOF)");
                        return ConnectionExit::Disconnected;
                    }
                    Ok(_) => {
                        if drop_next_chunk {
                            // Discard the first null-delimited chunk after a
                            // command/transition: it may hold CLI prompt/echo
                            // text buffered before the first binary frame.
                            drop_next_chunk = false;
                            buf.clear();
                            continue;
                        }

                        // Strip the trailing COBS `\0` terminator to leave just
                        // the COBS body.
                        if buf.last() == Some(&DELIMITER) {
                            buf.pop();
                        }

                        // Only forward to consumers while a session is active.
                        // After `POST /api/control/stop` flips
                        // `collection_running` to false, this drops any
                        // tail-of-session bytes (in-flight CSI frames, post-`q`
                        // boot text, command echoes) on the floor instead of
                        // leaking them. The buffer is still cleared below so the
                        // framer keeps draining serial input.
                        let still_collecting = collection_running.load(Ordering::SeqCst);

                        if still_collecting && !buf.is_empty() {
                            frames_in += 1;
                            if matches!(current_mode, OutputMode::Dump | OutputMode::Both) {
                                if let (Some(sink), Some(chip)) = (sink.as_mut(), chip.as_ref()) {
                                    match csi::decode(&buf, chip.variant) {
                                        Ok(decoded) => {
                                            let host_rx = chrono::Utc::now().timestamp_micros();
                                            if let Err(e) = sink.push(decoded, host_rx) {
                                                tracing::error!("Parquet write error: {e}");
                                            }
                                        }
                                        Err(e) => {
                                            decode_errors += 1;
                                            // Hex-dump the first few raw frames to
                                            // diagnose wire mismatches (run with
                                            // RUST_LOG=debug).
                                            if decode_errors <= 3 {
                                                let hex: String = buf
                                                    .iter()
                                                    .map(|b| format!("{b:02x}"))
                                                    .collect();
                                                tracing::warn!(
                                                    "Decode error #{decode_errors} on {port_path}: {e}; cobs_len={} frame_hex={hex}",
                                                    buf.len(),
                                                );
                                            } else if decode_errors.is_power_of_two() {
                                                tracing::warn!(
                                                    "Failed to decode CSI frame on {port_path} ({} total); check firmware/chip wire compatibility",
                                                    decode_errors,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            if matches!(current_mode, OutputMode::Stream | OutputMode::Both)
                                && csi_tx.send(buf.clone()).is_ok()
                            {
                                frames_broadcast += 1;
                            }
                        }
                        buf.clear();
                    }
                    Err(e) => {
                        tracing::error!("Serial read error on {port_path}: {e}");
                        return ConnectionExit::Disconnected;
                    }
                }
            }

            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(cmd) => {
                        tracing::debug!("→ ESP32: {cmd}");
                        // Command echoes are text but the wire framing is
                        // null-delimited; drop the next chunk so echoes don't
                        // mix with binary payload.
                        drop_next_chunk = true;
                        let line = format!("{cmd}\r\n");
                        if let Err(e) = writer.write_all(line.as_bytes()).await {
                            tracing::error!("Serial write error: {e}");
                            return ConnectionExit::Disconnected;
                        }
                        // NB: deliberately no `flush()` here. tokio-serial maps
                        // `flush` to a blocking `tcdrain()` that runs on the
                        // tokio worker thread and waits for the UART/USB FIFO to
                        // physically empty. If the device browns out or its USB
                        // endpoint wedges (common with several native-USB boards
                        // on one bus), `tcdrain` blocks forever and takes the
                        // worker with it — enough of them freezes the whole
                        // runtime. `write_all` has already handed the bytes to
                        // the kernel via a non-blocking write; the USB stack
                        // sends them without us draining.
                    }
                    None => {
                        return ConnectionExit::CommandChannelClosed;
                    }
                }
            }

            req = info_request_rx.recv() => {
                let Some(responder) = req else { continue };

                if collection_running.load(Ordering::SeqCst) {
                    let _ = responder.send(Err(
                        "collection is running; CLI is locked until stop".to_string(),
                    ));
                    continue;
                }

                // The info block is text — drop any partial COBS chunk
                // straddling our text exchange.
                drop_next_chunk = true;
                // Discard any partial CSI frame the framer was accumulating;
                // the info exchange runs in line-mode below.
                buf.clear();

                match do_info_exchange(&mut writer, &mut reader, &[]).await {
                    Ok(info) => {
                        chip = ChipInfo::from_info(&info);
                        firmware_verified.store(true, Ordering::SeqCst);
                        *device_info.lock().await = Some(info.clone());
                        *dev.fault.lock().await = None;
                        dev.recovery_cycles.store(0, Ordering::SeqCst);
                        let _ = responder.send(Ok(info));
                    }
                    Err(InfoExchangeError::Soft(msg)) => {
                        firmware_verified.store(false, Ordering::SeqCst);
                        *device_info.lock().await = None;
                        let _ = responder.send(Err(msg));
                    }
                    Err(InfoExchangeError::BootFault(fault)) => {
                        firmware_verified.store(false, Ordering::SeqCst);
                        *device_info.lock().await = None;
                        let mut slot = dev.fault.lock().await;
                        if slot.as_deref() != Some(fault.as_str()) {
                            tracing::error!("Chip fault detected on {port_path}: {fault}");
                            *slot = Some(fault.clone());
                        }
                        drop(slot);
                        let _ = responder.send(Err(fault));
                    }
                    Err(InfoExchangeError::Hard(msg)) => {
                        firmware_verified.store(false, Ordering::SeqCst);
                        *device_info.lock().await = None;
                        let _ = responder.send(Err(msg));
                        return ConnectionExit::Disconnected;
                    }
                }
            }
        }
    }
}

/// Last-resort hardware reset for a native USB-Serial-JTAG device that will
/// not verify: pulse RTS (→ chip EN) with DTR held low on a short-lived
/// second fd, ending with both lines released. The chip reboots into the app
/// and re-enumerates; the pinned reconnect loop (same path) or the supervisor
/// (new path, followed by MAC) picks it up and re-verifies.
///
/// This is deliberately NOT done on every connect — a USB-JTAG reset is the
/// operation implicated in the C5 post-flash wedge — only as an escalation
/// after CLI-level recovery (`q`, `restart`) has failed.
async fn usb_jtag_reset_pulse(port_path: &str, baud: u32) -> Result<(), String> {
    let mut port = tokio_serial::new(port_path, baud)
        .open_native_async()
        .map_err(|e| format!("open for reset pulse failed: {e}"))?;
    #[cfg(unix)]
    {
        let _ = port.set_exclusive(false);
    }
    // esptool's `HardReset` for USB-Serial-JTAG: DTR stays LOW the whole time
    // (BOOT strap released → the chip boots the APP) while RTS pulses the
    // emulated EN line. This is the reset espflash issues after flashing.
    //
    // Deliberately NOT `UsbJtagSerialReset` (idle → DTR high → RTS through
    // (1,1) → release): that sequence latches the BOOT strap low first, so the
    // chip reboots into ROM download mode — silent, unresponsive to `info`,
    // and indistinguishable from a dead board (observed live: a C5 parked in
    // Secure Download Mode by exactly that sequence).
    let _ = port.write_data_terminal_ready(false);
    port.write_request_to_send(true) // EN low — chip held in reset
        .map_err(|e| format!("RTS assert failed: {e}"))?;
    let _ = port.write_data_terminal_ready(false); // (Windows only latches DTR on RTS writes)
    sleep(Duration::from_millis(100)).await;
    port.write_request_to_send(false) // EN released → chip boots the app
        .map_err(|e| format!("RTS deassert failed: {e}"))?;
    Ok(())
}

/// Record a terminal, human-action-required fault on the device (once — an
/// existing fault, e.g. a specific boot-signature classification, wins).
/// Surfaced through `GET /api/devices` and the webclient's FAULT banner;
/// cleared automatically on the next successful firmware verification.
async fn declare_terminal_fault(dev: &DeviceHandle, port_path: &str) {
    let mut slot = dev.fault.lock().await;
    if slot.is_none() {
        let fault = "unresponsive after every automated recovery step (firmware restart, \
             USB-JTAG reset pulse) — the application does not boot. Press the board's \
             EN/reset button, replug the cable, or power-cycle the port (e.g. `uhubctl -a \
             cycle` on a power-switchable hub)."
            .to_string();
        tracing::error!("Chip fault declared on {port_path}: {fault}");
        *slot = Some(fault);
    }
}

/// Bring a possibly-streaming device back to a responsive CLI.
///
/// Sends a stop (`q`) and discards whatever the device emits until the stream
/// goes idle (or a short cap elapses). Used on native USB-Serial-JTAG chips,
/// which skip the RTS reset and so may still be flooding CSI from a previous
/// session — without this, that flood buries the `info` request and the device
/// never verifies.
/// Returns the first bytes of whatever was drained (capped) so the caller can
/// pass them to [`do_info_exchange`] as boot context — a chip that resets when
/// the port is opened prints its ROM banner *here*, not during the exchange,
/// and the fault classifier needs to see it.
async fn quiesce_stale_stream<W, R>(
    writer: &mut W,
    reader: &mut BufReader<R>,
    port_path: &str,
) -> Vec<u8>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    // Cap on retained (not drained) bytes: enough for several ROM boot
    // banners; a CSI flood past this point is discarded as before.
    const RETAIN_CAP: usize = 8 * 1024;

    // No `flush()`: it maps to a blocking `tcdrain()` on the worker thread that
    // hangs forever if the device's USB endpoint has wedged. `write_all` already
    // delivered the bytes to the kernel. The write itself is also bounded — a
    // wedged CDC endpoint can block even the kernel handoff indefinitely, which
    // used to freeze this device's task with no log output.
    match tokio::time::timeout(Duration::from_secs(2), writer.write_all(b"q\r\n")).await {
        Ok(_) => {}
        Err(_) => {
            tracing::warn!(
                "Quiesce write on {port_path} timed out — USB endpoint may be wedged"
            );
            return Vec::new();
        }
    }

    let mut scratch = [0u8; 2048];
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    let mut drained = 0usize;
    let mut retained: Vec<u8> = Vec::new();
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        match tokio::time::timeout(deadline - now, reader.read(&mut scratch)).await {
            Ok(Ok(0)) => break, // EOF
            Ok(Ok(n)) => {
                // Discard the backlog but keep the head for fault classification.
                if retained.len() < RETAIN_CAP {
                    let take = n.min(RETAIN_CAP - retained.len());
                    retained.extend_from_slice(&scratch[..take]);
                }
                drained += n;
            }
            Ok(Err(_)) => break,
            Err(_) => break, // idle — stream has quiesced
        }
    }
    if drained > 0 {
        tracing::info!("Drained {drained} bytes of stale stream on {port_path} before verify");
    }
    retained
}

/// Issue a single `info` command on the link and read until the `END-INFO`
/// sentinel arrives or [`INFO_RESPONSE_TIMEOUT`] elapses. Returns
/// `Soft` errors when the link is healthy but the firmware is not (or not
/// `esp-csi-cli-rs`); `Hard` errors when the I/O itself failed.
///
/// `boot_context` is what a preceding [`quiesce_stale_stream`] drained — a
/// chip that resets when the port opens prints its ROM banner there, before
/// this exchange starts, so timeouts are classified over both buffers.
async fn do_info_exchange<W, R>(
    writer: &mut W,
    reader: &mut BufReader<R>,
    boot_context: &[u8],
) -> Result<DeviceInfo, InfoExchangeError>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    // Leading CRLF: if the CLI's line editor holds stale partial input (junk
    // typed before the server attached, or a half-written command from a
    // dropped session), `info` would be appended to it and rejected. The bare
    // newline makes the editor consume the junk line (as a failed command)
    // and present a fresh prompt for `info`.
    // Bounded: a wedged USB CDC endpoint can block `write_all` indefinitely,
    // which used to freeze this device's task with no log output.
    match tokio::time::timeout(Duration::from_secs(2), writer.write_all(b"\r\ninfo\r\n")).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return Err(InfoExchangeError::Hard(format!("Serial write error: {e}")));
        }
        Err(_) => {
            return Err(InfoExchangeError::Hard(
                "serial write timed out — USB endpoint may be wedged".to_string(),
            ));
        }
    }
    // No `flush()`: tokio-serial's flush is a blocking `tcdrain()` that wedges
    // the worker thread if the device's USB endpoint stalls. The non-blocking
    // `write_all` above is sufficient to send the command.

    let deadline = tokio::time::Instant::now() + INFO_RESPONSE_TIMEOUT;
    let mut info_buf: Vec<u8> = Vec::new();

    // On timeout, classify whatever bytes DID arrive (boot context from the
    // quiesce drain plus this window): a wedged chip is not silent — it spews
    // ROM boot banners before going quiet or looping.
    let classify_timeout = |rx: &[u8]| {
        let mut all = Vec::with_capacity(boot_context.len() + rx.len());
        all.extend_from_slice(boot_context);
        all.extend_from_slice(rx);
        match detect_boot_fault(&all) {
            Some(fault) => InfoExchangeError::BootFault(fault),
            None => InfoExchangeError::Soft(
                "info command timed out; firmware may not be esp-csi-cli-rs".to_string(),
            ),
        }
    };

    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(classify_timeout(&info_buf));
        }
        let remaining = deadline.saturating_duration_since(now);
        let read_fut = reader.read_until(b'\n', &mut info_buf);
        match tokio::time::timeout(remaining, read_fut).await {
            Ok(Ok(0)) => {
                return Err(InfoExchangeError::Hard(
                    "serial closed during info exchange".to_string(),
                ));
            }
            Ok(Ok(_)) => {
                if find_subsequence(&info_buf, b"END-INFO").is_some() {
                    return parse_info_block(&info_buf).map_err(InfoExchangeError::Soft);
                }
            }
            Ok(Err(e)) => {
                return Err(InfoExchangeError::Hard(format!("Serial read error: {e}")));
            }
            Err(_) => {
                return Err(classify_timeout(&info_buf));
            }
        }
    }
}

/// Parse the firmware-identification block emitted by the device-side
/// `info` command. The block is delimited by `ESP-CSI-CLI/<version>` (start)
/// and `END-INFO` (end), with `key=value` lines in between.
fn parse_info_block(buf: &[u8]) -> Result<DeviceInfo, String> {
    let text = String::from_utf8_lossy(buf);
    let lines: Vec<&str> = text.lines().map(str::trim).collect();

    let start = lines
        .iter()
        .position(|l| l.starts_with("ESP-CSI-CLI/"))
        .ok_or_else(|| {
            "info magic prefix 'ESP-CSI-CLI/' not seen — firmware is not esp-csi-cli-rs"
                .to_string()
        })?;
    let end = lines
        .iter()
        .skip(start)
        .position(|l| *l == "END-INFO")
        .map(|p| start + p)
        .ok_or_else(|| "END-INFO sentinel not seen in info block".to_string())?;

    let banner_version = lines[start]
        .strip_prefix("ESP-CSI-CLI/")
        .unwrap_or("")
        .to_string();

    let mut info = DeviceInfo {
        banner_version,
        name: None,
        version: None,
        chip: None,
        mac: None,
        protocol: None,
        features: Vec::new(),
    };

    for line in &lines[start + 1..end] {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k {
            "name" => info.name = Some(v.to_string()),
            "version" => info.version = Some(v.to_string()),
            "chip" => info.chip = Some(v.to_string()),
            "mac" => info.mac = Some(v.to_string()),
            "protocol" => info.protocol = v.parse().ok(),
            "features" => {
                info.features = v
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }

    Ok(info)
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Reconcile the Parquet sink with the active output mode and session path.
///
/// Opens a sink when dumping is active and a session path is set (requires a
/// known chip so frames can be decoded); drops the sink — finalizing the file —
/// when switching to stream-only.
fn sync_parquet_sink(
    mode: &OutputMode,
    session_path: &Option<String>,
    chip: Option<&ChipInfo>,
    sink: &mut Option<ParquetSink>,
    profile: &Arc<dyn CsiProfile>,
) {
    match mode {
        OutputMode::Dump | OutputMode::Both => {
            if sink.is_none() {
                if let Some(path) = session_path {
                    let Some(chip) = chip else {
                        tracing::error!(
                            "Cannot open Parquet dump {path}: chip not identified or unsupported; \
                             frames cannot be decoded. Streaming (if enabled) is unaffected."
                        );
                        return;
                    };
                    match ParquetSink::open(path, &chip.name, profile.clone()) {
                        Ok(s) => {
                            tracing::info!("Opened Parquet dump: {path} (chip {})", chip.name);
                            *sink = Some(s);
                        }
                        Err(e) => {
                            tracing::error!("Failed to open Parquet dump {path}: {e}");
                        }
                    }
                }
            }
        }
        OutputMode::Stream => {
            if sink.take().is_some() {
                // Dropping the sink finalizes the Parquet file.
                tracing::info!("Switched to stream mode — Parquet file finalized");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::detect_boot_fault;

    /// The exact banner an ESP32-C5 spews when wedged in the USB-JTAG reset
    /// loop (rst:0x15) after flashing — repeats until a USB power cycle.
    const C5_WEDGE_BANNER: &str = "ESP-ROM:esp32c5-eco2-20250121\r\n\
        Build:Jan 21 2025\r\n\
        rst:0x15 (USB_UART_HPSYS),boot:0x10 (SPI_FAST_FLASH_BOOT)\r\n\
        Core0 Saved PC:0x40038598\r\n";

    #[test]
    fn usb_jtag_reset_loop_detected_on_repeat() {
        let two = format!("{C5_WEDGE_BANNER}{C5_WEDGE_BANNER}");
        let fault = detect_boot_fault(two.as_bytes()).expect("wedge not detected");
        assert!(fault.contains("USB-JTAG reset loop"));
        assert!(fault.contains("power-cycle") || fault.contains("Power-cycle"));
    }

    #[test]
    fn boot_banner_followed_by_cli_is_not_a_fault() {
        // A single ROM banner that then reaches the CLI is a normal reset (e.g. after espflash
        // or a port open). `detect_boot_fault` only runs after an info-exchange timeout, so the
        // meaningful "not a fault" case is a banner with CLI output after it — the `ESP-CSI-CLI`
        // marker suppresses the lone-wedge classification.
        let recovered = format!("{C5_WEDGE_BANNER}ESP-CSI-CLI/0.7.0\r\nchip=esp32c5\r\nEND-INFO\r\n");
        assert!(detect_boot_fault(recovered.as_bytes()).is_none());
    }

    #[test]
    fn lone_wedge_banner_after_timeout_is_a_fault() {
        // A lone USB_UART_HPSYS banner with no CLI after it (only reached once the info exchange
        // already timed out) means the chip reset via USB-JTAG and never came back — a wedge.
        let fault = detect_boot_fault(C5_WEDGE_BANNER.as_bytes()).expect("lone wedge not detected");
        assert!(fault.contains("USB-JTAG-triggered reset"));
    }

    #[test]
    fn download_mode_detected() {
        let rx = b"ESP-ROM:esp32c5-eco2-20250121\r\nwaiting for download\r\n";
        let fault = detect_boot_fault(rx).expect("download mode not detected");
        assert!(fault.contains("download mode"));
    }

    #[test]
    fn generic_boot_loop_detected() {
        let banner = "ESP-ROM:esp32c5-eco2-20250121\r\nrst:0x3 (RTC_SW_SYS_RST)\r\n";
        let rx = banner.repeat(3);
        let fault = detect_boot_fault(rx.as_bytes()).expect("boot loop not detected");
        assert!(fault.contains("boot loop"));
    }

    #[test]
    fn normal_cli_output_is_healthy() {
        assert!(detect_boot_fault(b"ESP-CSI-CLI/0.7.0\r\nchip=esp32c5\r\nEND-INFO\r\n").is_none());
        assert!(detect_boot_fault(b"").is_none());
    }
}
