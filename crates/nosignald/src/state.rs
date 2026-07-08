//! Persisted daemon state (`daemon.toml` in the state dir): active profile,
//! loop-guard suspension, audio-restore bookkeeping, and any pending job that
//! must survive a daemon crash (E12).

use nosignal_core::{EdidId, OutputIdentity, Topology};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DaemonState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
    /// Set by the loop guard; cleared by an explicit profile apply.
    #[serde(default)]
    pub suspended: bool,
    #[serde(default)]
    pub audio_restore: Vec<AudioRestore>,
    /// Crash-safe pending job: if the daemon dies mid-countdown, startup
    /// reverts an expired job (or re-arms the remainder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<PersistedPending>,
}

/// Audio sink to restore when the named output comes back.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioRestore {
    pub connector: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edid: Option<EdidId>,
    /// Default sink recorded just before the output was disabled.
    pub sink: String,
    /// What the default became after PipeWire failed over; used to detect
    /// whether the user changed sinks manually in the meantime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

impl AudioRestore {
    pub fn identity(&self) -> OutputIdentity {
        OutputIdentity {
            edid: self.edid.clone(),
            connector: self.connector.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedPending {
    pub job_id: u64,
    /// Unix seconds after which the job auto-reverts.
    pub deadline_epoch: u64,
    /// JSON-serialized [`Topology`] to restore on revert.
    pub prior_json: String,
}

impl PersistedPending {
    pub fn prior(&self) -> Option<Topology> {
        serde_json::from_str(&self.prior_json).ok()
    }
}

impl DaemonState {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}
