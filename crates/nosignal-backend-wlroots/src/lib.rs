//! wlroots display backend for NoSignal (Sway, Hyprland, river, …) via the
//! `zwlr_output_manager_v1` Wayland protocol.
//!
//! A dedicated reader thread owns the event queue and mirrors protocol state
//! (heads, modes, config serial) into shared state; snapshot/apply/watch are
//! served from that mirror. Configuration results map onto the backend
//! contract: `cancelled` = the serial went stale (re-snapshot and retry),
//! `failed` = the compositor rejected the layout.
//!
//! wlroots compositors do not persist output configuration themselves (that
//! is kanshi's job) — the daemon's re-assert engine is the persistence story.

#[cfg(target_os = "linux")]
mod imp;
#[cfg(target_os = "linux")]
pub use imp::WlrootsBackend;
