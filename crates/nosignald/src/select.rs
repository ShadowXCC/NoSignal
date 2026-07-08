//! Display backend auto-detection.
//!
//! Order: `NOSIGNAL_BACKEND` env override → probe chain. `XDG_CURRENT_DESKTOP`
//! puts the desktop-owned backend first (GNOME and KDE own display config
//! through their own APIs on both X11 and Wayland); generic Wayland (wlroots)
//! and X11 (RandR) are the fallbacks. Every candidate is *probed* — a hint
//! that doesn't answer falls through to the next candidate.

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
        #[cfg(target_os = "linux")]
        "kde" => Ok(Arc::new(nosignal_backend_kde::KdeBackend::new().await?)),
        #[cfg(target_os = "linux")]
        "wlroots" => Ok(Arc::new(
            nosignal_backend_wlroots::WlrootsBackend::new().await?,
        )),
        #[cfg(target_os = "linux")]
        "x11" => Ok(Arc::new(nosignal_backend_x11::X11Backend::new().await?)),
        #[cfg(target_os = "windows")]
        "windows" => Ok(Arc::new(nosignal_backend_win::WindowsBackend::new().await?)),
        other => Err(BackendError::Unavailable(format!(
            "unknown backend '{other}' (NOSIGNAL_BACKEND)"
        ))),
    }
}

#[cfg(target_os = "linux")]
async fn auto() -> Result<Arc<dyn DisplayBackend>, BackendError> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_uppercase();
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11_session = std::env::var_os("DISPLAY").is_some();

    let mut candidates: Vec<&str> = Vec::new();
    let push = |c: &'static str, list: &mut Vec<&str>| {
        if !list.contains(&c) {
            list.push(c);
        }
    };
    if desktop.contains("GNOME") {
        push("gnome", &mut candidates);
    }
    if desktop.contains("KDE") {
        push("kde", &mut candidates);
    }
    if wayland {
        // Unhinted Wayland: GNOME/KDE probes are cheap and cover sessions
        // with a missing XDG_CURRENT_DESKTOP; wlroots is the generic path.
        push("gnome", &mut candidates);
        push("kde", &mut candidates);
        push("wlroots", &mut candidates);
    } else if x11_session {
        // Pure X11 session: desktop-owned backends first (already pushed via
        // hints), RandR as the generic fallback. Under Wayland, DISPLAY is
        // XWayland and RandR only sees virtual outputs — deliberately skipped.
        push("gnome", &mut candidates);
        push("kde", &mut candidates);
        push("x11", &mut candidates);
    }

    let mut errors = Vec::new();
    for name in &candidates {
        match by_name(name).await {
            Ok(backend) => {
                tracing::info!("auto-detected display backend: {name}");
                return Ok(backend);
            }
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    Err(BackendError::Unavailable(format!(
        "no display backend answered (XDG_CURRENT_DESKTOP='{desktop}', wayland={wayland}, \
         x11={x11_session}); tried: [{}] — set NOSIGNAL_BACKEND to override",
        errors.join("; ")
    )))
}

#[cfg(target_os = "windows")]
async fn auto() -> Result<Arc<dyn DisplayBackend>, BackendError> {
    Ok(Arc::new(nosignal_backend_win::WindowsBackend::new().await?))
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
async fn auto() -> Result<Arc<dyn DisplayBackend>, BackendError> {
    Err(BackendError::Unavailable(
        "no display backend for this platform; see CONTRIBUTING.md".into(),
    ))
}
