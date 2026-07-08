//! Error types shared across the workspace.

use thiserror::Error;

/// Errors from display backends.
#[derive(Debug, Error)]
pub enum BackendError {
    /// The configuration serial in the plan no longer matches the display
    /// server's state. Callers should re-snapshot and retry once.
    #[error("stale configuration serial; re-snapshot and retry")]
    StaleSerial,

    #[error("invalid layout: {0}")]
    InvalidLayout(String),

    #[error("unknown output: {0}")]
    UnknownOutput(String),

    /// The display did not come back after enable — typically a DisplayPort
    /// monitor in deep sleep that dropped off the bus. Actionable, not fatal.
    #[error("output '{0}' did not respond; wake the monitor (power button) and retry")]
    OutputUnresponsive(String),

    #[error("display server error: {0}")]
    Server(String),

    /// This backend cannot run in the current session (wrong compositor,
    /// missing protocol/API). Auto-detection treats this as "try the next one".
    #[error("backend unavailable: {0}")]
    Unavailable(String),
}

/// Errors resolving a target (alias / connector / EDID substring) or a stored
/// identity against live outputs.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResolveError {
    #[error("no output matches '{0}'")]
    NotFound(String),

    #[error(
        "ambiguous target '{target}': matches {}; use a connector name to disambiguate",
        candidates.join(", ")
    )]
    Ambiguous {
        target: String,
        candidates: Vec<String>,
    },
}
