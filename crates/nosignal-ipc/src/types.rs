//! Wire types shared between daemon and clients (serialized as JSON).

use nosignal_core::guards::RiskClass;
use nosignal_core::profile::ProfileWarning;
use serde::{Deserialize, Serialize};

/// Options for enable/disable/toggle requests.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct SetOpts {
    /// Suppress the interactive-risk requirement: the caller has already
    /// confirmed the risk with the user (or is a script that accepts it).
    #[serde(default)]
    pub force: bool,
    /// Never arm a revert timer. Refused (GuardRefused) for mandatory-timer
    /// risks (last active display, built-in panel).
    #[serde(default)]
    pub no_timer: bool,
    /// Arm a revert timer of this many seconds. `None` = client default
    /// (no timer for routine changes; 20 s where mandatory).
    #[serde(default)]
    pub revert_secs: Option<u64>,
}

/// Result of a state-changing request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SetOutcome {
    /// Change applied and persisted.
    Applied {
        #[serde(default)]
        warnings: Vec<ProfileWarning>,
    },
    /// Nothing to do.
    AlreadyInState,
    /// Change applied temporarily; confirm or it reverts at the deadline.
    Pending {
        job_id: u64,
        deadline_secs: u64,
        risk: RiskClass,
        #[serde(default)]
        warnings: Vec<ProfileWarning>,
    },
    /// A guard refused the request (e.g. `no_timer` on a mandatory-timer
    /// risk, or risk not acknowledged with `force` over IPC).
    GuardRefused { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub name: String,
    pub hotkey: Option<String>,
    pub active: bool,
    /// Live state no longer matches this profile (only meaningful when active).
    pub drifted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfilesInfo {
    pub profiles: Vec<ProfileInfo>,
    pub active: Option<String>,
    /// The active profile was suspended by the loop guard.
    pub suspended: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInfo {
    pub job_id: u64,
    pub deadline_secs: u64,
    pub risk: RiskClass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusInfo {
    pub version: String,
    pub backend: String,
    pub active_profile: Option<String>,
    pub suspended: bool,
    pub drifted: bool,
    pub pending: Option<PendingInfo>,
    pub outputs_total: usize,
    pub outputs_enabled: usize,
}

/// Machine-readable error classification carried across the IPC boundary so
/// clients can keep their exit-code semantics (2 = ambiguous, 3 = guarded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorKind {
    NotFound,
    Ambiguous,
    Guard,
    Backend,
    Other,
}

/// JSON envelope for daemon method results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum Envelope<T> {
    Ok(T),
    Err { kind: ApiErrorKind, message: String },
}

impl<T> Envelope<T> {
    pub fn into_result(self) -> Result<T, crate::IpcError> {
        match self {
            Envelope::Ok(v) => Ok(v),
            Envelope::Err { kind, message } => Err(crate::IpcError::Api { kind, message }),
        }
    }
}

/// Events broadcast by the daemon (DBus signals / pipe notifications).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DaemonEvent {
    OutputsChanged,
    PendingChange { job_id: u64, seconds_remaining: u64 },
    PendingResolved { job_id: u64, kept: bool },
    ProfileApplied { name: String },
    ProfileDrifted { name: String },
    ProfileSuspended { name: String },
}
