//! X11 display backend for NoSignal, via the RandR extension (x11rb).
//!
//! Covers XFCE, MATE, Cinnamon, and any other X11 desktop that doesn't own
//! display configuration through a higher-level API. Disable = detach the
//! output's CRTC; enable = assign a free CRTC with the planned mode. The
//! screen (framebuffer) is resized around CRTC changes the same way xrandr
//! does: disable shrinking CRTCs → set screen size → configure CRTCs.
//!
//! X11 has no native layout persistence — the daemon's re-assert engine (at
//! login and on hotplug) is the persistence story on these desktops.

#[cfg(target_os = "linux")]
mod imp;
#[cfg(target_os = "linux")]
pub use imp::X11Backend;
