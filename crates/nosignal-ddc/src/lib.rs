//! Opt-in DDC/CI monitor power control for NoSignal.
//!
//! Output deactivation makes a display sleep on *its own* no-signal timer
//! (seconds for monitors, minutes for TVs). For monitors that speak DDC/CI,
//! writing VCP feature 0xD6 (Power Mode) puts the panel into standby
//! immediately instead.
//!
//! Hard-won realities encoded here:
//! - DDC/CI is a per-path lottery: it frequently dies through docks,
//!   adapters, MST hubs, and KVMs. Everything is **capability-probed and
//!   per-output opt-in**; failures are logged, never fatal.
//! - The wake path is *always* "re-enable the output and let the returning
//!   signal wake the panel" — some monitors accept sleep over DDC but cannot
//!   be woken by it, so NoSignal never DDC-wakes.
//! - TVs generally don't speak DDC/CI at all (that world is HDMI-CEC, a
//!   future plugin); TVs also sleep by themselves, which is why this feature
//!   targets desktop monitors.
//!
//! All functions are blocking (i2c / OS monitor APIs) — call via
//! `spawn_blocking` from async contexts.

use nosignal_core::OutputIdentity;
use thiserror::Error;

/// MCCS VCP code: Power Mode.
pub const VCP_POWER_MODE: u8 = 0xD6;
/// D6 value 4 = soft off. Wakes when the video signal returns (unlike 5,
/// hard off, which needs the power button).
pub const POWER_SOFT_OFF: u16 = 0x04;

#[derive(Debug, Error)]
pub enum DdcError {
    #[error("no DDC/CI-capable display matches {0}")]
    NotFound(String),
    #[error("DDC/CI communication failed: {0}")]
    Comm(String),
    #[error("DDC/CI is not supported on this platform")]
    Unsupported,
}

/// Result of probing an output for DDC/CI power control.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    /// Monitor's current power mode value (1 = on).
    pub current_power: u16,
    /// Backend path description (i2c device / OS handle), for bug reports.
    pub via: String,
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod imp {
    use super::*;
    use ddc_hi::{Ddc, Display};

    fn matches(display: &Display, identity: &OutputIdentity) -> bool {
        // Strongest: parse the monitor's own EDID and compare triples.
        if let Some(bytes) = &display.info.edid_data
            && let Some(parsed) = nosignal_core::edid::parse(bytes)
            && let Some(wanted) = &identity.edid
        {
            return parsed == *wanted;
        }
        // Fallback: manufacturer + serial string from the backend.
        if let Some(wanted) = &identity.edid {
            let mfg_match = display
                .info
                .manufacturer_id
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(&wanted.vendor));
            let serial_match = display
                .info
                .serial_number
                .as_deref()
                .is_some_and(|s| s == wanted.serial);
            return mfg_match && serial_match;
        }
        false
    }

    fn find(identity: &OutputIdentity) -> Result<Display, DdcError> {
        for display in Display::enumerate() {
            if matches(&display, identity) {
                return Ok(display);
            }
        }
        Err(DdcError::NotFound(identity.to_string()))
    }

    pub fn probe(identity: &OutputIdentity) -> Result<ProbeResult, DdcError> {
        let mut display = find(identity)?;
        let value = display
            .handle
            .get_vcp_feature(VCP_POWER_MODE)
            .map_err(|e| DdcError::Comm(e.to_string()))?;
        Ok(ProbeResult {
            current_power: value.value(),
            via: display.info.backend.to_string(),
        })
    }

    pub fn standby(identity: &OutputIdentity) -> Result<(), DdcError> {
        let mut display = find(identity)?;
        display
            .handle
            .set_vcp_feature(VCP_POWER_MODE, POWER_SOFT_OFF)
            .map_err(|e| DdcError::Comm(e.to_string()))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod imp {
    use super::*;

    pub fn probe(_identity: &OutputIdentity) -> Result<ProbeResult, DdcError> {
        Err(DdcError::Unsupported)
    }

    pub fn standby(_identity: &OutputIdentity) -> Result<(), DdcError> {
        Err(DdcError::Unsupported)
    }
}

/// Probe whether the display behind `identity` answers DDC/CI power queries.
pub fn probe(identity: &OutputIdentity) -> Result<ProbeResult, DdcError> {
    imp::probe(identity)
}

/// Put the display behind `identity` into soft-off immediately.
pub fn standby(identity: &OutputIdentity) -> Result<(), DdcError> {
    imp::standby(identity)
}
