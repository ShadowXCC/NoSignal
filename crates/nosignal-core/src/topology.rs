//! Topology snapshot and layout-plan types shared by all backends.
//!
//! Backends apply **whole layouts, never single-output deltas** — that is how
//! Mutter and Windows CCD actually work and it sidesteps ordering bugs.

use crate::identity::OutputIdentity;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A display mode. Refresh is stored in millihertz so modes compare exactly
/// (59.997 Hz == 59_997 mHz).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
}

impl Mode {
    pub fn refresh_hz(&self) -> f64 {
        f64::from(self.refresh_mhz) / 1000.0
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}@{:.3}", self.width, self.height, self.refresh_hz())
    }
}

/// A user-supplied mode specification like `3840x2160@120` or `3840x2160`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeSpec {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: Option<f64>,
}

impl ModeSpec {
    /// Parse `WxH` or `WxH@Hz` (Hz may be fractional).
    pub fn parse(spec: &str) -> Option<Self> {
        let (dims, refresh) = match spec.split_once('@') {
            Some((d, r)) => (d, Some(r.trim().parse::<f64>().ok()?)),
            None => (spec, None),
        };
        let (w, h) = dims.trim().split_once(['x', 'X'])?;
        Some(Self {
            width: w.trim().parse().ok()?,
            height: h.trim().parse().ok()?,
            refresh_hz: refresh,
        })
    }

    /// Pick the best matching mode: exact resolution required, then closest
    /// refresh to the requested one (or the highest refresh if unspecified).
    pub fn best_match(&self, modes: &[Mode]) -> Option<Mode> {
        let candidates = modes
            .iter()
            .filter(|m| m.width == self.width && m.height == self.height);
        match self.refresh_hz {
            Some(want) => candidates.min_by_key(|m| {
                let diff = m.refresh_hz() - want;
                (diff.abs() * 1000.0) as u64
            }),
            None => candidates.max_by_key(|m| m.refresh_mhz),
        }
        .copied()
    }
}

/// Output rotation/reflection, matching the eight wl_output/Mutter transforms.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transform {
    #[default]
    Normal,
    Rot90,
    Rot180,
    Rot270,
    Flipped,
    FlippedRot90,
    FlippedRot180,
    FlippedRot270,
}

impl Transform {
    /// Numeric encoding shared by Mutter and wayland (0..=7).
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        use Transform::*;
        Some(match v {
            0 => Normal,
            1 => Rot90,
            2 => Rot180,
            3 => Rot270,
            4 => Flipped,
            5 => FlippedRot90,
            6 => FlippedRot180,
            7 => FlippedRot270,
            _ => return None,
        })
    }
}

/// One physical output as reported by a backend snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub identity: OutputIdentity,
    /// User-facing alias ("TV", "left") attached by the daemon from config.
    pub alias: Option<String>,
    /// Human-readable name from the backend, e.g. "Samsung Electric Company 55\"".
    pub display_name: String,
    /// Internal panel (eDP/LVDS/DSI) — gets the extra disable guard.
    pub builtin: bool,
    /// Currently driving a signal (part of the desktop).
    pub enabled: bool,
    /// Current mode when enabled.
    pub mode: Option<Mode>,
    pub preferred_mode: Option<Mode>,
    /// All modes the display advertises.
    pub modes: Vec<Mode>,
    /// Logical position in the desktop coordinate space.
    pub position: (i32, i32),
    pub scale: f64,
    pub transform: Transform,
    pub primary: bool,
}

/// Heuristic for internal panels, shared by backends that only expose the
/// connector name.
pub fn connector_is_builtin(connector: &str) -> bool {
    let c = connector.to_ascii_lowercase();
    c.starts_with("edp") || c.starts_with("lvds") || c.starts_with("dsi")
}

/// Full snapshot of the current display configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Topology {
    /// Backend-opaque configuration serial. Plans derived from a snapshot
    /// carry it back so backends can reject stale applies.
    pub serial: String,
    pub outputs: Vec<Output>,
}

impl Topology {
    pub fn enabled_outputs(&self) -> impl Iterator<Item = &Output> {
        self.outputs.iter().filter(|o| o.enabled)
    }

    pub fn enabled_count(&self) -> usize {
        self.enabled_outputs().count()
    }

    pub fn find_connector(&self, connector: &str) -> Option<&Output> {
        self.outputs
            .iter()
            .find(|o| o.identity.connector == connector)
    }
}

/// Desired state for one output within a [`LayoutPlan`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedOutput {
    pub identity: OutputIdentity,
    pub enabled: bool,
    /// `None` = keep the output's current mode, falling back to preferred.
    pub mode: Option<Mode>,
    pub position: (i32, i32),
    pub scale: f64,
    pub transform: Transform,
    pub primary: bool,
}

impl PlannedOutput {
    pub fn from_output(o: &Output) -> Self {
        Self {
            identity: o.identity.clone(),
            enabled: o.enabled,
            mode: o.mode.or(o.preferred_mode),
            position: o.position,
            scale: if o.scale > 0.0 { o.scale } else { 1.0 },
            transform: o.transform,
            primary: o.primary,
        }
    }
}

/// A full desired topology: one entry per known output. Disable = entry with
/// `enabled: false` (backends translate to their own semantics, e.g. Mutter
/// omits the monitor from `logical_monitors`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutPlan {
    /// Serial of the [`Topology`] this plan was derived from.
    pub serial: String,
    pub outputs: Vec<PlannedOutput>,
}

impl LayoutPlan {
    /// Start from the current state: every output planned as-is.
    pub fn from_topology(t: &Topology) -> Self {
        Self {
            serial: t.serial.clone(),
            outputs: t.outputs.iter().map(PlannedOutput::from_output).collect(),
        }
    }

    pub fn find_connector_mut(&mut self, connector: &str) -> Option<&mut PlannedOutput> {
        self.outputs
            .iter_mut()
            .find(|o| o.identity.connector == connector)
    }

    /// Set the enabled state of the output with the given connector.
    pub fn set_enabled(&mut self, connector: &str, enabled: bool) -> bool {
        match self.find_connector_mut(connector) {
            Some(o) => {
                o.enabled = enabled;
                true
            }
            None => false,
        }
    }

    pub fn enabled_count(&self) -> usize {
        self.outputs.iter().filter(|o| o.enabled).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_spec_parses_and_picks_best_match() {
        let modes = [
            Mode {
                width: 3840,
                height: 2160,
                refresh_mhz: 60_000,
            },
            Mode {
                width: 3840,
                height: 2160,
                refresh_mhz: 119_880,
            },
            Mode {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
            },
        ];

        let spec = ModeSpec::parse("3840x2160@120").unwrap();
        assert_eq!(spec.best_match(&modes).unwrap().refresh_mhz, 119_880);

        let spec = ModeSpec::parse("3840x2160").unwrap();
        assert_eq!(spec.best_match(&modes).unwrap().refresh_mhz, 119_880);

        let spec = ModeSpec::parse("2560x1440").unwrap();
        assert!(spec.best_match(&modes).is_none());

        assert!(ModeSpec::parse("garbage").is_none());
    }

    #[test]
    fn transform_round_trips() {
        for v in 0..=7u8 {
            assert_eq!(Transform::from_u8(v).unwrap().to_u8(), v);
        }
        assert!(Transform::from_u8(8).is_none());
    }

    #[test]
    fn builtin_connector_heuristic() {
        assert!(connector_is_builtin("eDP-1"));
        assert!(connector_is_builtin("LVDS-1"));
        assert!(!connector_is_builtin("DP-1"));
        assert!(!connector_is_builtin("HDMI-A-1"));
    }
}
