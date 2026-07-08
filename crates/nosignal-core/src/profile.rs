//! Profile schema (versioned from day one) and profile → plan resolution.
//!
//! A profile stores a **full layout** for the outputs it names — enabled
//! state plus mode, position, scale, transform, and primary — and is silent
//! about outputs it doesn't name (they keep their current state). Stored as
//! TOML in the platform config directory; the daemon owns the file.

use crate::error::ResolveError;
use crate::identity::{EdidId, OutputIdentity, resolve_identity};
use crate::layout::normalize;
use crate::topology::{LayoutPlan, ModeSpec, Topology, Transform};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

pub const SCHEMA_VERSION: u32 = 1;

/// The on-disk profile store (`profiles.toml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileStore {
    pub schema_version: u32,
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

impl Default for ProfileStore {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            profiles: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Hot-switchable by name.
    pub name: String,
    /// Optional global hotkey, e.g. `<Super>F9`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey: Option<String>,
    #[serde(default)]
    pub outputs: Vec<ProfileOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioPrefs>,
}

/// Desired state for one output. Both identifiers are stored; resolution is
/// EDID first, connector fallback (see [`crate::identity`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileOutput {
    /// User-facing alias ("TV", "left").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edid: Option<EdidId>,
    pub connector: String,
    pub enabled: bool,
    /// Mode spec like `3840x2160@120`; omitted = current/preferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<(i32, i32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<bool>,
}

impl ProfileOutput {
    pub fn identity(&self) -> OutputIdentity {
        OutputIdentity {
            edid: self.edid.clone(),
            connector: self.connector.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioPrefs {
    /// Sink to restore when this profile's displays come back, e.g. a
    /// PipeWire node name on Linux.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_sink: Option<String>,
}

/// Non-fatal notes produced while resolving a profile against live outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileWarning {
    /// A profile entry matched nothing — display not connected. The profile
    /// stays "silent" about it.
    NotConnected { connector: String },
    /// EDID matched on a different connector than stored (replugged); the
    /// stored connector should be updated on next save.
    ConnectorMigrated { stored: String, live: String },
    /// The stored mode spec doesn't match any mode the display advertises;
    /// current/preferred mode is used instead.
    ModeUnavailable { connector: String, mode: String },
}

impl Profile {
    /// Capture the current topology as a full-layout profile.
    pub fn capture(name: impl Into<String>, topology: &Topology) -> Self {
        Self {
            name: name.into(),
            hotkey: None,
            outputs: topology
                .outputs
                .iter()
                .map(|o| ProfileOutput {
                    alias: o.alias.clone(),
                    edid: o.identity.edid.clone(),
                    connector: o.identity.connector.clone(),
                    enabled: o.enabled,
                    mode: o.mode.map(|m| m.to_string()),
                    position: Some(o.position),
                    scale: Some(o.scale),
                    transform: Some(o.transform),
                    primary: Some(o.primary),
                })
                .collect(),
            audio: None,
        }
    }

    /// Resolve this profile against a live topology into an applicable plan.
    ///
    /// Outputs the profile doesn't name keep their current state; entries for
    /// disconnected displays produce warnings, not errors. Ambiguity (two live
    /// outputs matching one entry) is a hard error.
    pub fn to_plan(
        &self,
        topology: &Topology,
    ) -> Result<(LayoutPlan, Vec<ProfileWarning>), ResolveError> {
        let mut plan = LayoutPlan::from_topology(topology);
        let mut warnings = Vec::new();

        for entry in &self.outputs {
            let wanted = entry.identity();
            let Some(live) = resolve_identity(&wanted, &topology.outputs)? else {
                warnings.push(ProfileWarning::NotConnected {
                    connector: entry.connector.clone(),
                });
                continue;
            };
            if live.identity.connector != entry.connector {
                warnings.push(ProfileWarning::ConnectorMigrated {
                    stored: entry.connector.clone(),
                    live: live.identity.connector.clone(),
                });
            }

            let planned = plan
                .find_connector_mut(&live.identity.connector)
                .expect("plan covers every live output");
            planned.enabled = entry.enabled;
            if let Some(spec_str) = &entry.mode {
                match ModeSpec::parse(spec_str).and_then(|s| s.best_match(&live.modes)) {
                    Some(mode) => planned.mode = Some(mode),
                    None => warnings.push(ProfileWarning::ModeUnavailable {
                        connector: live.identity.connector.clone(),
                        mode: spec_str.clone(),
                    }),
                }
            }
            if let Some(pos) = entry.position {
                planned.position = pos;
            }
            if let Some(scale) = entry.scale {
                planned.scale = scale;
            }
            if let Some(t) = entry.transform {
                planned.transform = t;
            }
            if let Some(p) = entry.primary {
                planned.primary = p;
            }
        }

        normalize(&mut plan);
        Ok((plan, warnings))
    }

    /// Whether the live topology already satisfies this profile's demands
    /// (used for drift detection). Only enabled-state is compared — mode and
    /// position drift is tolerated; on/off is what NoSignal owns.
    pub fn is_satisfied_by(&self, topology: &Topology) -> Result<bool, ResolveError> {
        for entry in &self.outputs {
            if let Some(live) = resolve_identity(&entry.identity(), &topology.outputs)?
                && live.enabled != entry.enabled
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("failed to read profile store: {0}")]
    Io(#[from] std::io::Error),
    #[error("profile store is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("failed to serialize profile store: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error(
        "profile store schema version {found} is newer than supported ({supported}); upgrade NoSignal"
    )]
    SchemaTooNew { found: u32, supported: u32 },
}

impl ProfileStore {
    pub fn load(path: &Path) -> Result<Self, StoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let store: Self = toml::from_str(&text)?;
        if store.schema_version > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: store.schema_version,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(store)
    }

    /// Atomic save: write a temp file in the same directory, then rename.
    pub fn save(&self, path: &Path) -> Result<(), StoreError> {
        let text = toml::to_string_pretty(self)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Insert or replace a profile by name.
    pub fn upsert(&mut self, profile: Profile) {
        match self.profiles.iter_mut().find(|p| p.name == profile.name) {
            Some(slot) => *slot = profile,
            None => self.profiles.push(profile),
        }
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.name != name);
        self.profiles.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{EdidId, OutputIdentity};
    use crate::topology::{Mode, Output};

    fn mode(w: u32, h: u32, mhz: u32) -> Mode {
        Mode {
            width: w,
            height: h,
            refresh_mhz: mhz,
        }
    }

    fn tv_edid() -> EdidId {
        EdidId {
            vendor: "SAM".into(),
            product: "0x7201".into(),
            serial: "777".into(),
        }
    }

    fn sample_topology() -> Topology {
        Topology {
            serial: "42".into(),
            outputs: vec![
                Output {
                    identity: OutputIdentity::new("DP-1", None),
                    display_name: "Dell U2723QE".into(),
                    enabled: true,
                    mode: Some(mode(3840, 2160, 60_000)),
                    modes: vec![mode(3840, 2160, 60_000), mode(1920, 1080, 60_000)],
                    scale: 1.0,
                    primary: true,
                    ..Output::default()
                },
                Output {
                    identity: OutputIdentity::new("HDMI-A-1", Some(tv_edid())),
                    alias: Some("TV".into()),
                    display_name: "Samsung TV".into(),
                    enabled: true,
                    mode: Some(mode(3840, 2160, 60_000)),
                    modes: vec![mode(3840, 2160, 60_000), mode(3840, 2160, 119_880)],
                    position: (3840, 0),
                    scale: 2.0,
                    ..Output::default()
                },
            ],
        }
    }

    #[test]
    fn capture_and_toml_round_trip() {
        let topo = sample_topology();
        let mut store = ProfileStore::default();
        store.upsert(Profile::capture("desk", &topo));

        let dir = std::env::temp_dir().join(format!("nosignal-test-{}", std::process::id()));
        let path = dir.join("profiles.toml");
        store.save(&path).unwrap();
        let loaded = ProfileStore::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(store, loaded);
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        let desk = loaded.find("desk").unwrap();
        assert_eq!(desk.outputs.len(), 2);
        assert_eq!(desk.outputs[1].edid, Some(tv_edid()));
    }

    #[test]
    fn newer_schema_is_rejected() {
        let dir = std::env::temp_dir().join(format!("nosignal-test-schema-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("profiles.toml");
        std::fs::write(&path, "schema_version = 999\n").unwrap();
        let err = ProfileStore::load(&path).unwrap_err();
        std::fs::remove_dir_all(&dir).ok();
        assert!(matches!(err, StoreError::SchemaTooNew { found: 999, .. }));
    }

    #[test]
    fn profile_plan_disables_tv_and_keeps_unnamed_outputs() {
        let topo = sample_topology();
        let movie_off = Profile {
            name: "desk".into(),
            outputs: vec![ProfileOutput {
                alias: Some("TV".into()),
                edid: Some(tv_edid()),
                connector: "HDMI-A-1".into(),
                enabled: false,
                ..ProfileOutput::default()
            }],
            ..Profile::default()
        };

        let (plan, warnings) = movie_off.to_plan(&topo).unwrap();
        assert!(warnings.is_empty());
        let tv = plan
            .outputs
            .iter()
            .find(|o| o.identity.connector == "HDMI-A-1")
            .unwrap();
        assert!(!tv.enabled);
        let dp = plan
            .outputs
            .iter()
            .find(|o| o.identity.connector == "DP-1")
            .unwrap();
        assert!(dp.enabled, "unnamed output keeps current state");
        assert!(dp.primary);
    }

    #[test]
    fn replugged_tv_matches_by_edid_and_reports_migration() {
        let mut topo = sample_topology();
        // TV moved to HDMI-A-2.
        topo.outputs[1].identity.connector = "HDMI-A-2".into();

        let profile = Profile {
            name: "p".into(),
            outputs: vec![ProfileOutput {
                edid: Some(tv_edid()),
                connector: "HDMI-A-1".into(),
                enabled: false,
                ..ProfileOutput::default()
            }],
            ..Profile::default()
        };

        let (plan, warnings) = profile.to_plan(&topo).unwrap();
        assert!(
            !plan
                .outputs
                .iter()
                .find(|o| o.identity.connector == "HDMI-A-2")
                .unwrap()
                .enabled
        );
        assert_eq!(
            warnings,
            vec![ProfileWarning::ConnectorMigrated {
                stored: "HDMI-A-1".into(),
                live: "HDMI-A-2".into(),
            }]
        );
    }

    #[test]
    fn disconnected_entry_warns_but_applies_rest() {
        let mut topo = sample_topology();
        topo.outputs.remove(1); // TV unplugged
        let profile = Profile {
            name: "p".into(),
            outputs: vec![ProfileOutput {
                edid: Some(tv_edid()),
                connector: "HDMI-A-1".into(),
                enabled: false,
                ..ProfileOutput::default()
            }],
            ..Profile::default()
        };
        let (plan, warnings) = profile.to_plan(&topo).unwrap();
        assert_eq!(plan.outputs.len(), 1);
        assert_eq!(
            warnings,
            vec![ProfileWarning::NotConnected {
                connector: "HDMI-A-1".into()
            }]
        );
    }

    #[test]
    fn mode_pin_resolves_to_closest_advertised_mode() {
        let topo = sample_topology();
        let profile = Profile {
            name: "movie".into(),
            outputs: vec![ProfileOutput {
                edid: Some(tv_edid()),
                connector: "HDMI-A-1".into(),
                enabled: true,
                mode: Some("3840x2160@120".into()),
                ..ProfileOutput::default()
            }],
            ..Profile::default()
        };
        let (plan, warnings) = profile.to_plan(&topo).unwrap();
        assert!(warnings.is_empty());
        let tv = plan
            .outputs
            .iter()
            .find(|o| o.identity.connector == "HDMI-A-1")
            .unwrap();
        assert_eq!(tv.mode.unwrap().refresh_mhz, 119_880);
    }

    #[test]
    fn drift_detection_compares_enabled_state_only() {
        let topo = sample_topology();
        let profile = Profile {
            name: "p".into(),
            outputs: vec![ProfileOutput {
                edid: Some(tv_edid()),
                connector: "HDMI-A-1".into(),
                enabled: false,
                ..ProfileOutput::default()
            }],
            ..Profile::default()
        };
        // TV is currently enabled but the profile wants it off → drifted.
        assert!(!profile.is_satisfied_by(&topo).unwrap());

        let mut off = topo.clone();
        off.outputs[1].enabled = false;
        assert!(profile.is_satisfied_by(&off).unwrap());
    }
}
