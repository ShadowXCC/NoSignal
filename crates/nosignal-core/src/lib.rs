//! Core types, backend trait, display identity matching, and profile schema
//! for NoSignal.
//!
//! Everything in this crate is platform-independent. Display-server specifics
//! live in the `nosignal-backend-*` crates behind [`backend::DisplayBackend`];
//! all state and orchestration lives in the `nosignald` daemon.

pub mod backend;
pub mod edid;
pub mod error;
pub mod guards;
pub mod identity;
pub mod layout;
pub mod profile;
pub mod remembered;
pub mod topology;

pub use backend::{ApplyMode, Capabilities, DisplayBackend, TopologyEvent};
pub use error::{BackendError, ResolveError};
pub use identity::{EdidId, OutputIdentity};
pub use topology::{LayoutPlan, Mode, Output, PlannedOutput, Topology, Transform};

/// Reverse-DNS application identity.
///
/// TODO(identity): placeholder namespace — finalized together with the
/// repository URL at repo creation. Every identifier must be derived from the
/// constants below; nothing else in the tree may hard-code the namespace.
pub const APP_ID: &str = "org.nosignal.NoSignal";
/// DBus well-known name of the daemon (Linux IPC + automation surface).
pub const DBUS_NAME: &str = "org.nosignal.Daemon1";
/// DBus object path of the daemon.
pub const DBUS_PATH: &str = "/org/nosignal/Daemon1";
/// Windows named-pipe path of the daemon.
pub const PIPE_NAME: &str = r"\\.\pipe\nosignal-daemon";
