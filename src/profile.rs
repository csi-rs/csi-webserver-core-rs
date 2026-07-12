//! Pluggable capability seam for optional, out-of-tree CSI extensions.
//!
//! The open core knows only the protocols, presets, and data-format labels it
//! ships with. A [`CsiProfile`] lets an embedder extend those sets at
//! construction time without the core naming any of the extra values: the
//! server injects one profile (default [`StandardCsiProfile`], a no-op) into
//! [`AppState`](crate::state::AppState) / the per-device serial task, and the
//! config routes and Parquet sink consult it where they previously hard-coded
//! chip-specific behaviour.
//!
//! All methods are defaulted so the standard profile is empty and an embedder
//! overrides only what it needs.

use crate::models::CsiConfigSection;

/// Optional, embedder-supplied CSI capability extensions.
///
/// Injected once at server construction and shared (`Arc`) across every route
/// handler and per-device serial task. The default implementations make the
/// standard profile a transparent no-op.
pub trait CsiProfile: Send + Sync {
    /// Extra `set-protocol` values accepted in addition to the core set, e.g.
    /// a newer PHY the base build does not name.
    fn extra_protocols(&self) -> &'static [&'static str] {
        &[]
    }

    /// Resolve a named CSI preset to the cached config section it applies.
    /// `None` means the core does not recognise the preset name.
    fn resolve_preset(&self, name: &str) -> Option<CsiConfigSection> {
        let _ = name;
        None
    }

    /// Full `set-csi …` command line for a named preset. `None` means the core
    /// does not recognise the preset name.
    fn preset_cli(&self, name: &str) -> Option<String> {
        let _ = name;
        None
    }

    /// Human-readable `data_format` label for a raw `cur_bb_format` value the
    /// core does not label itself. `None` falls back to the decoded
    /// [`RxCsiFmt`](crate::csi::RxCsiFmt) name.
    fn label_format(&self, cur_bb_format: u32) -> Option<&'static str> {
        let _ = cur_bb_format;
        None
    }
}

/// The default no-op profile: no extra protocols, presets, or format labels.
pub struct StandardCsiProfile;

impl CsiProfile for StandardCsiProfile {}
