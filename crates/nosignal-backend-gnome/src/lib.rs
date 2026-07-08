//! GNOME display backend for NoSignal, via the `org.gnome.Mutter.DisplayConfig`
//! DBus API on the session bus.
//!
//! Works on GNOME over both Wayland and X11 (Mutter owns display config in
//! both). The interface is nominally private but has been stable for many
//! years; the call shapes here follow the working consumers `gnome-randr` and
//! `gnome-monitor-config`.
//!
//! Key mechanics encoded here:
//! - "disable" = omit the monitor from `logical_monitors` in
//!   `ApplyMonitorsConfig` — there is no `disabled: true` flag;
//! - the config `serial` must come from the latest `GetCurrentState`; a
//!   mismatch is [`BackendError::StaleSerial`] and the caller re-snapshots;
//! - logical monitor scale values must be ones Mutter offers for the mode —
//!   never computed by us;
//! - `MonitorsChanged` drives [`TopologyEvent::Changed`].

#[cfg(target_os = "linux")]
mod imp;
#[cfg(target_os = "linux")]
pub use imp::GnomeBackend;
