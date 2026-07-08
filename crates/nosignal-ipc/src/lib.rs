//! Shared IPC plumbing for NoSignal clients and daemon.
//!
//! The wire types here are the daemon's public API surface, carried as JSON
//! strings over DBus (Linux) or a named pipe (Windows, M4). Clients use
//! [`DaemonClient`]; the daemon implements the other side in `nosignald`.

pub mod types;

#[cfg(target_os = "linux")]
pub mod dbus_client;
#[cfg(target_os = "windows")]
pub mod pipe_client;

use async_trait::async_trait;
use thiserror::Error;
use types::*;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("daemon not reachable: {0}")]
    Unreachable(String),
    #[error("{message}")]
    Api {
        kind: types::ApiErrorKind,
        message: String,
    },
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// Client-side view of the daemon API. One implementation per transport.
#[async_trait]
pub trait DaemonClient: Send + Sync {
    /// Subscribe to daemon events (GUI/tray refresh signal).
    async fn events(
        &self,
    ) -> Result<futures::stream::BoxStream<'static, types::DaemonEvent>, IpcError>;
    async fn list_outputs(&self) -> Result<nosignal_core::Topology, IpcError>;
    async fn set_output_enabled(
        &self,
        target: &str,
        enabled: Option<bool>,
        opts: SetOpts,
    ) -> Result<SetOutcome, IpcError>;
    async fn confirm_pending(&self) -> Result<bool, IpcError>;
    async fn revert_pending(&self) -> Result<bool, IpcError>;
    async fn list_profiles(&self) -> Result<ProfilesInfo, IpcError>;
    async fn apply_profile(&self, name: &str) -> Result<SetOutcome, IpcError>;
    async fn save_profile(&self, name: &str) -> Result<(), IpcError>;
    async fn delete_profile(&self, name: &str) -> Result<bool, IpcError>;
    async fn set_alias(&self, alias: &str, target: &str) -> Result<(), IpcError>;
    async fn get_status(&self) -> Result<StatusInfo, IpcError>;
    async fn quit(&self) -> Result<(), IpcError>;
}

/// Connect to the daemon over the platform transport.
#[cfg(target_os = "linux")]
pub async fn connect() -> Result<Box<dyn DaemonClient>, IpcError> {
    Ok(Box::new(dbus_client::DbusClient::connect().await?))
}

#[cfg(target_os = "windows")]
pub async fn connect() -> Result<Box<dyn DaemonClient>, IpcError> {
    Ok(Box::new(pipe_client::PipeClient::connect().await?))
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub async fn connect() -> Result<Box<dyn DaemonClient>, IpcError> {
    Err(IpcError::Unreachable(
        "no IPC transport for this platform".into(),
    ))
}
