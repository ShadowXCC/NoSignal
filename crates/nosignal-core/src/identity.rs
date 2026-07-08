//! Output identity and the EDID-first matching rules.
//!
//! Displays are identified by two things captured together: the EDID triple
//! (vendor, product, serial — survives replugging into a different port) and
//! the connector name (fallback when EDID is absent or garbage). Resolution is
//! always EDID first, connector second, and a hard error on ambiguity — never
//! a guess.

use crate::error::ResolveError;
use crate::topology::Output;
use serde::{Deserialize, Serialize};
use std::fmt;

/// EDID identity triple as reported by the display.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EdidId {
    /// PNP vendor id, e.g. `SAM`, `DEL`, `GSM`.
    pub vendor: String,
    /// Product code, backend-formatted (typically hex, e.g. `0x7201`).
    pub product: String,
    /// Serial string. Frequently `0` or duplicated on identical monitors —
    /// which is exactly why ambiguity is a hard error.
    pub serial: String,
}

impl fmt::Display for EdidId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.vendor, self.product, self.serial)
    }
}

/// Full identity of an output: EDID when available, connector always.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutputIdentity {
    pub edid: Option<EdidId>,
    /// Connector name, e.g. `HDMI-A-1`, `DP-3`, `eDP-1`, `\\.\DISPLAY1`.
    pub connector: String,
}

/// How strongly two identities matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchQuality {
    None,
    /// Connector names are equal (EDID unavailable on one side or mismatched).
    Connector,
    /// EDID triples are equal — the authoritative match.
    Edid,
}

impl OutputIdentity {
    pub fn new(connector: impl Into<String>, edid: Option<EdidId>) -> Self {
        Self {
            edid,
            connector: connector.into(),
        }
    }

    /// EDID-first comparison against another identity (e.g. a profile entry).
    pub fn match_quality(&self, other: &OutputIdentity) -> MatchQuality {
        if let (Some(a), Some(b)) = (&self.edid, &other.edid)
            && a == b
        {
            return MatchQuality::Edid;
        }
        if self.connector == other.connector {
            return MatchQuality::Connector;
        }
        MatchQuality::None
    }
}

/// Resolve a stored identity (profile entry) against the live outputs.
///
/// Returns `Ok(None)` when nothing matches — for profile resolution that
/// simply means the display is not connected right now, which is not an error.
/// Returns [`ResolveError::Ambiguous`] when more than one live output matches
/// at the same (best) quality, e.g. two identical monitors reporting serial 0.
pub fn resolve_identity<'a>(
    wanted: &OutputIdentity,
    outputs: &'a [Output],
) -> Result<Option<&'a Output>, ResolveError> {
    for quality in [MatchQuality::Edid, MatchQuality::Connector] {
        let hits: Vec<&Output> = outputs
            .iter()
            .filter(|o| o.identity.match_quality(wanted) == quality)
            .collect();
        match hits.len() {
            0 => continue,
            1 => return Ok(Some(hits[0])),
            _ => {
                return Err(ResolveError::Ambiguous {
                    target: wanted.to_string(),
                    candidates: hits.iter().map(|o| o.identity.connector.clone()).collect(),
                });
            }
        }
    }
    Ok(None)
}

impl fmt::Display for OutputIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.edid {
            Some(e) => write!(f, "{} ({e})", self.connector),
            None => write!(f, "{}", self.connector),
        }
    }
}

/// Resolve a user-supplied target string — alias, connector, or EDID
/// substring — against the live outputs.
///
/// Priority: exact alias (case-insensitive) → exact connector
/// (case-insensitive) → case-insensitive substring of `vendor product serial`.
/// Multiple hits at the winning tier is a hard [`ResolveError::Ambiguous`].
pub fn resolve_target<'a>(target: &str, outputs: &'a [Output]) -> Result<&'a Output, ResolveError> {
    let needle = target.trim().to_lowercase();

    let alias_hits: Vec<&Output> = outputs
        .iter()
        .filter(|o| o.alias.as_ref().is_some_and(|a| a.to_lowercase() == needle))
        .collect();
    if let Some(found) = single(&alias_hits, target)? {
        return Ok(found);
    }

    let connector_hits: Vec<&Output> = outputs
        .iter()
        .filter(|o| o.identity.connector.to_lowercase() == needle)
        .collect();
    if let Some(found) = single(&connector_hits, target)? {
        return Ok(found);
    }

    let edid_hits: Vec<&Output> = outputs
        .iter()
        .filter(|o| {
            o.identity
                .edid
                .as_ref()
                .is_some_and(|e| e.to_string().to_lowercase().contains(&needle))
        })
        .collect();
    if let Some(found) = single(&edid_hits, target)? {
        return Ok(found);
    }

    Err(ResolveError::NotFound(target.to_string()))
}

fn single<'a>(hits: &[&'a Output], target: &str) -> Result<Option<&'a Output>, ResolveError> {
    match hits.len() {
        0 => Ok(None),
        1 => Ok(Some(hits[0])),
        _ => Err(ResolveError::Ambiguous {
            target: target.to_string(),
            candidates: hits.iter().map(|o| o.identity.connector.clone()).collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::Output;

    fn edid(vendor: &str, product: &str, serial: &str) -> EdidId {
        EdidId {
            vendor: vendor.into(),
            product: product.into(),
            serial: serial.into(),
        }
    }

    fn out(connector: &str, edid_id: Option<EdidId>, alias: Option<&str>) -> Output {
        Output {
            identity: OutputIdentity::new(connector, edid_id),
            alias: alias.map(String::from),
            ..Output::default()
        }
    }

    #[test]
    fn target_resolution_prefers_alias_then_connector_then_edid() {
        let outputs = vec![
            out("DP-1", Some(edid("DEL", "0xa0b1", "12345")), Some("main")),
            out("HDMI-A-1", Some(edid("SAM", "0x7201", "777")), Some("TV")),
        ];
        assert_eq!(
            resolve_target("tv", &outputs).unwrap().identity.connector,
            "HDMI-A-1"
        );
        assert_eq!(
            resolve_target("dp-1", &outputs).unwrap().identity.connector,
            "DP-1"
        );
        assert_eq!(
            resolve_target("sam", &outputs).unwrap().identity.connector,
            "HDMI-A-1"
        );
        assert!(matches!(
            resolve_target("nothing", &outputs),
            Err(ResolveError::NotFound(_))
        ));
    }

    #[test]
    fn edid_match_wins_over_connector_when_replugged() {
        // Profile stored the TV on HDMI-A-1, but it was replugged into
        // HDMI-A-2 while an unrelated display took HDMI-A-1.
        let tv = edid("SAM", "0x7201", "777");
        let wanted = OutputIdentity::new("HDMI-A-1", Some(tv.clone()));
        let outputs = vec![
            out("HDMI-A-1", Some(edid("DEL", "0xa0b1", "1")), None),
            out("HDMI-A-2", Some(tv), None),
        ];
        let hit = resolve_identity(&wanted, &outputs).unwrap().unwrap();
        assert_eq!(hit.identity.connector, "HDMI-A-2");
    }

    #[test]
    fn connector_fallback_when_edid_absent() {
        let wanted = OutputIdentity::new("DP-2", None);
        let outputs = vec![out("DP-2", Some(edid("AUS", "0x11", "0")), None)];
        let hit = resolve_identity(&wanted, &outputs).unwrap().unwrap();
        assert_eq!(hit.identity.connector, "DP-2");
    }

    #[test]
    fn duplicate_edids_are_a_hard_ambiguity_error() {
        // Two identical monitors reporting serial 0.
        let dup = edid("GSM", "0x5b09", "0");
        let wanted = OutputIdentity::new("DP-9", Some(dup.clone()));
        let outputs = vec![
            out("DP-1", Some(dup.clone()), None),
            out("DP-2", Some(dup), None),
        ];
        let err = resolve_identity(&wanted, &outputs).unwrap_err();
        match err {
            ResolveError::Ambiguous { candidates, .. } => {
                assert_eq!(candidates, vec!["DP-1", "DP-2"]);
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn disconnected_profile_entry_resolves_to_none() {
        let wanted = OutputIdentity::new("HDMI-A-1", Some(edid("SAM", "0x7201", "777")));
        let outputs = vec![out("DP-1", Some(edid("DEL", "0xa0b1", "1")), None)];
        assert!(resolve_identity(&wanted, &outputs).unwrap().is_none());
    }
}
