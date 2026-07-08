//! Daemon configuration (`config.toml` in the config dir): output aliases
//! and tunables. Owned by the daemon; the GUI edits it through the daemon.

use nosignal_core::identity::MatchQuality;
use nosignal_core::{EdidId, OutputIdentity, Topology};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Auto-revert timer length used when a timer is mandatory and the client
    /// didn't specify one.
    #[serde(default = "default_revert_secs")]
    pub revert_secs: u64,
    #[serde(default)]
    pub aliases: Vec<AliasEntry>,
}

fn default_revert_secs() -> u64 {
    20
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AliasEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edid: Option<EdidId>,
    pub connector: String,
}

impl AliasEntry {
    pub fn identity(&self) -> OutputIdentity {
        OutputIdentity {
            edid: self.edid.clone(),
            connector: self.connector.clone(),
        }
    }
}

impl DaemonConfig {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_else(|| Self {
                revert_secs: default_revert_secs(),
                aliases: Vec::new(),
            })
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

    /// Attach configured aliases to a topology snapshot (EDID-first match).
    pub fn attach_aliases(&self, topology: &mut Topology) {
        for output in &mut topology.outputs {
            for quality in [MatchQuality::Edid, MatchQuality::Connector] {
                if let Some(entry) = self
                    .aliases
                    .iter()
                    .find(|a| output.identity.match_quality(&a.identity()) == quality)
                {
                    output.alias = Some(entry.name.clone());
                    break;
                }
            }
        }
    }

    /// Set or replace an alias for the given identity.
    pub fn set_alias(&mut self, name: &str, identity: &OutputIdentity) {
        self.aliases.retain(|a| a.name != name);
        self.aliases.push(AliasEntry {
            name: name.to_string(),
            edid: identity.edid.clone(),
            connector: identity.connector.clone(),
        });
    }
}
