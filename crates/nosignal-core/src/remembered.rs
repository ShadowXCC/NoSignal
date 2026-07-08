//! Remembered per-output layouts.
//!
//! When NoSignal disables an output it stores the output's full logical
//! config (mode, position, scale, transform, primary) so re-enable restores
//! it exactly — the display server forgets disabled outputs' placement.
//! Entries are keyed by output identity with the usual EDID-first matching.

use crate::identity::MatchQuality;
use crate::topology::PlannedOutput;
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RememberedLayouts {
    #[serde(default)]
    pub outputs: Vec<PlannedOutput>,
}

#[derive(Debug, Error)]
pub enum RememberedError {
    #[error("failed to read remembered layouts: {0}")]
    Io(#[from] std::io::Error),
    #[error("remembered layouts file is corrupt: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("failed to serialize remembered layouts: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl RememberedLayouts {
    pub fn load(path: &Path) -> Result<Self, RememberedError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: &Path) -> Result<(), RememberedError> {
        let text = toml::to_string_pretty(self)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Best entry for an identity: EDID match first, connector fallback.
    pub fn find(&self, identity: &crate::identity::OutputIdentity) -> Option<&PlannedOutput> {
        for quality in [MatchQuality::Edid, MatchQuality::Connector] {
            if let Some(hit) = self
                .outputs
                .iter()
                .find(|o| o.identity.match_quality(identity) == quality)
            {
                return Some(hit);
            }
        }
        None
    }

    /// Insert or replace the entry matching this output's identity.
    pub fn upsert(&mut self, planned: PlannedOutput) {
        let slot = self
            .outputs
            .iter_mut()
            .find(|o| o.identity.match_quality(&planned.identity) >= MatchQuality::Connector);
        match slot {
            Some(existing) => *existing = planned,
            None => self.outputs.push(planned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{EdidId, OutputIdentity};
    use crate::topology::{Mode, Transform};

    fn planned(connector: &str, edid: Option<EdidId>, pos: (i32, i32)) -> PlannedOutput {
        PlannedOutput {
            identity: OutputIdentity::new(connector, edid),
            enabled: true,
            mode: Some(Mode {
                width: 3840,
                height: 2160,
                refresh_mhz: 119_880,
            }),
            position: pos,
            scale: 2.0,
            transform: Transform::Normal,
            primary: false,
        }
    }

    fn tv_edid() -> EdidId {
        EdidId {
            vendor: "SAM".into(),
            product: "0x7201".into(),
            serial: "777".into(),
        }
    }

    #[test]
    fn round_trip_and_edid_first_lookup() {
        let mut store = RememberedLayouts::default();
        store.upsert(planned("HDMI-A-1", Some(tv_edid()), (3840, 0)));

        let dir = std::env::temp_dir().join(format!("nosignal-rem-{}", std::process::id()));
        let path = dir.join("remembered.toml");
        store.save(&path).unwrap();
        let loaded = RememberedLayouts::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(store, loaded);

        // Same panel replugged into a different connector still matches.
        let moved = OutputIdentity::new("HDMI-A-2", Some(tv_edid()));
        let hit = loaded.find(&moved).unwrap();
        assert_eq!(hit.position, (3840, 0));
    }

    #[test]
    fn upsert_replaces_by_identity() {
        let mut store = RememberedLayouts::default();
        store.upsert(planned("HDMI-A-1", Some(tv_edid()), (3840, 0)));
        store.upsert(planned("HDMI-A-1", Some(tv_edid()), (0, 2160)));
        assert_eq!(store.outputs.len(), 1);
        assert_eq!(store.outputs[0].position, (0, 2160));
    }
}
