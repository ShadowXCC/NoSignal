//! Windows display backend for NoSignal, via the CCD (Connecting and
//! Configuring Displays) API: `QueryDisplayConfig` for snapshots,
//! `SetDisplayConfig` for applies.
//!
//! Semantics encoded here, learned from NirSoft MultiMonitorTool's changelog
//! and the CCD documentation:
//! - disable = clear `DISPLAYCONFIG_PATH_ACTIVE` on the target's path and
//!   apply with `SDC_SAVE_TO_DATABASE` (Windows-native persistence);
//! - enable = mark an available inactive path active with invalid mode
//!   indices and `SDC_ALLOW_CHANGES`, letting Windows resolve modes from its
//!   database (which remembers the monitor's last layout);
//! - applies are retried a bounded number of times — complex topologies can
//!   need more than one attempt, and Windows 11 24H2 can wedge until poked;
//! - the monitor's EDID is read from the registry (SetupAPI device path) so
//!   identity matching works the same as on Linux;
//! - Windows' "primary display" is whichever sits at (0,0): applying a plan
//!   translates all positions so the planned primary lands there.

#[cfg(target_os = "windows")]
mod imp;
#[cfg(target_os = "windows")]
pub use imp::WindowsBackend;
