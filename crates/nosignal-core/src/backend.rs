//! The display backend trait every platform implementation fulfills.

use crate::error::BackendError;
use crate::topology::{LayoutPlan, Topology};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

/// How an apply should stick, mirroring Mutter's method flag; backends map
/// the other platforms' equivalents (e.g. CCD's `SDC_SAVE_TO_DATABASE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApplyMode {
    /// Validate only; change nothing.
    Verify,
    /// Apply for this session only (reverts on session restart). Used while a
    /// pending change awaits confirmation.
    Temporary,
    /// Apply and persist via the platform's native mechanism where one exists.
    Persistent,
}

/// What a backend can natively do; the daemon compensates for the rest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Layouts persist across sessions via the platform itself
    /// (Mutter `monitors.xml`, Windows CCD database).
    pub native_persistence: bool,
    /// `watch()` delivers real events; `false` means the daemon must poll.
    pub events: bool,
}

/// Something changed in the display world. Deliberately payload-free: any
/// consumer must re-`snapshot()` anyway, because events race against state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TopologyEvent {
    /// Outputs connected/disconnected or configuration changed (hotplug,
    /// resume, another tool applying a layout, ...).
    Changed,
}

/// A display backend: the only thing that talks to the display server / OS.
///
/// Contract notes:
/// - `apply` takes **whole layouts**; a plan must contain an entry for every
///   output the backend reported in the snapshot the plan's serial came from.
/// - `apply` must reject plans with stale serials with
///   [`BackendError::StaleSerial`].
/// - Backends do not decide policy (guards, timers, persistence strategy) —
///   that is the daemon's job.
#[async_trait]
pub trait DisplayBackend: Send + Sync {
    /// Short stable name: `"gnome"`, `"kde"`, `"wlroots"`, `"x11"`,
    /// `"windows"`, `"mock"`.
    fn name(&self) -> &'static str;

    fn capabilities(&self) -> Capabilities;

    /// Current outputs and layout, with a fresh configuration serial.
    async fn snapshot(&self) -> Result<Topology, BackendError>;

    /// Apply a full desired layout.
    async fn apply(&self, plan: &LayoutPlan, mode: ApplyMode) -> Result<(), BackendError>;

    /// Subscribe to topology-change events.
    async fn watch(&self) -> Result<BoxStream<'static, TopologyEvent>, BackendError>;
}
