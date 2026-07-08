//! Fullscreen detection ("warn-and-defer"): before disabling an output, the
//! daemon asks the oracle whether a fullscreen app might be running; if so,
//! clients must confirm (force) first.
//!
//! Platform reality: Windows can answer via `SHQueryUserNotificationState`;
//! on GNOME/KDE Wayland this is impossible from an external process — the
//! auto-revert timer is the safety net there (documented limitation). A
//! future GNOME Shell companion extension can provide a real oracle.

pub trait FullscreenOracle: Send + Sync {
    /// Best-effort: `true` when a fullscreen/presentation app seems active.
    fn fullscreen_active(&self) -> bool;
}

/// Platforms without detection: never blocks anything.
pub struct NoopOracle;

impl FullscreenOracle for NoopOracle {
    fn fullscreen_active(&self) -> bool {
        false
    }
}

#[cfg(target_os = "windows")]
pub use win::WindowsOracle;

#[cfg(target_os = "windows")]
mod win {
    use super::FullscreenOracle;
    use windows::Win32::UI::Shell::{
        QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
        SHQueryUserNotificationState,
    };

    pub struct WindowsOracle;

    impl FullscreenOracle for WindowsOracle {
        fn fullscreen_active(&self) -> bool {
            match unsafe { SHQueryUserNotificationState() } {
                Ok(state) => matches!(
                    state,
                    QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE
                ),
                Err(_) => false,
            }
        }
    }
}
