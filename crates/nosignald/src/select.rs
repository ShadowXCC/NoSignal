//! Display backend auto-detection.
//!
//! Order: `NOSIGNAL_BACKEND` env override → platform detection. On Linux the
//! desktop is identified via `XDG_CURRENT_DESKTOP` first because GNOME and
//! KDE own display config through their own APIs on both X11 and Wayland;
//! generic Wayland (wlroots) and X11 (RandR) are the fallbacks (M3).

use crate::mock_file::FileMockBackend;
use crate::paths;
use nosignal_core::{BackendError, DisplayBackend};
use std::sync::Arc;

/// Detect and connect the display backend for this session.
pub async fn detect() -> Result<Arc<dyn DisplayBackend>, BackendError> {
    if let Ok(name) = std::env::var("NOSIGNAL_BACKEND") {
        return by_name(&name).await;
    }
    auto().await
}

async fn by_name(name: &str) -> Result<Arc<dyn DisplayBackend>, BackendError> {
    match name {
        "mock" => Ok(Arc::new(FileMockBackend::load_or_seed(
            paths::mock_state_path(),
        ))),
        #[cfg(target_os = "linux")]
        "gnome" => Ok(Arc::new(nosignal_backend_gnome::GnomeBackend::new().await?)),
        other => Err(BackendError::Unavailable(format!(
            "unknown backend '{other}' (NOSIGNAL_BACKEND)"
        ))),
    }
}

#[cfg(target_os = "linux")]
async fn auto() -> Result<Arc<dyn DisplayBackend>, BackendError> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let desktop_upper = desktop.to_uppercase();

    if desktop_upper.contains("GNOME") {
        return Ok(Arc::new(nosignal_backend_gnome::GnomeBackend::new().await?));
    }
    // KDE / wlroots / X11 backends land in M3.
    Err(BackendError::Unavailable(format!(
        "no backend for this session yet (XDG_CURRENT_DESKTOP='{desktop}'); \
         KDE, wlroots and X11 support arrive in M3 — set NOSIGNAL_BACKEND=mock to experiment"
    )))
}

#[cfg(target_os = "windows")]
async fn auto() -> Result<Arc<dyn DisplayBackend>, BackendError> {
    // Windows CCD backend lands in M4.
    Err(BackendError::Unavailable(
        "the Windows backend arrives in M4 — set NOSIGNAL_BACKEND=mock to experiment".into(),
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
async fn auto() -> Result<Arc<dyn DisplayBackend>, BackendError> {
    Err(BackendError::Unavailable(
        "no display backend for this platform; see CONTRIBUTING.md".into(),
    ))
}
