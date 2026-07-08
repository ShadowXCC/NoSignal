//! KDE Plasma display backend for NoSignal, via KScreen.
//!
//! v1 wraps the `kscreen-doctor` CLI (stable since Plasma 5.23, JSON output
//! with `-j`): snapshots parse the JSON, applies emit `output.<name>.*`
//! setter arguments, and topology watching polls for configuration changes.
//! A native `kde-output-management-v2` Wayland implementation is the planned
//! upgrade path once the CLI wrapper has proven the semantics.
//!
//! KScreen itself persists per-hardware-combination configurations, so
//! changes applied here survive re-login; the daemon re-assert loop is the
//! backstop.

#[cfg(target_os = "linux")]
mod imp;
#[cfg(target_os = "linux")]
pub use imp::KdeBackend;
