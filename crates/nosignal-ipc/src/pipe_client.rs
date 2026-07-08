//! Windows client transport: named pipe with newline-delimited JSON.

use crate::types::*;
use crate::{DaemonClient, IpcError};
use async_trait::async_trait;
use nosignal_core::Topology;
use serde::de::DeserializeOwned;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

pub struct PipeClient {
    io: tokio::sync::Mutex<(
        BufReader<ReadHalf<NamedPipeClient>>,
        WriteHalf<NamedPipeClient>,
    )>,
    next_id: AtomicU64,
}

impl PipeClient {
    pub async fn connect() -> Result<Self, IpcError> {
        let pipe = ClientOptions::new()
            .open(nosignal_core::PIPE_NAME)
            .map_err(|e| IpcError::Unreachable(format!("daemon pipe: {e}")))?;
        let (read, write) = tokio::io::split(pipe);
        Ok(Self {
            io: tokio::sync::Mutex::new((BufReader::new(read), write)),
            next_id: AtomicU64::new(1),
        })
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, IpcError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = serde_json::json!({ "id": id, "method": method, "params": params });
        let mut line = request.to_string();
        line.push('\n');

        let mut io = self.io.lock().await;
        io.1.write_all(line.as_bytes())
            .await
            .map_err(|e| IpcError::Unreachable(format!("pipe write: {e}")))?;
        let mut response = String::new();
        let n =
            io.0.read_line(&mut response)
                .await
                .map_err(|e| IpcError::Unreachable(format!("pipe read: {e}")))?;
        drop(io);
        if n == 0 {
            return Err(IpcError::Unreachable("daemon closed the pipe".into()));
        }
        let value: serde_json::Value =
            serde_json::from_str(&response).map_err(|e| IpcError::Protocol(e.to_string()))?;
        let payload = value
            .get("payload")
            .cloned()
            .ok_or_else(|| IpcError::Protocol("response missing payload".into()))?;
        let envelope: Envelope<T> =
            serde_json::from_value(payload).map_err(|e| IpcError::Protocol(e.to_string()))?;
        envelope.into_result()
    }
}

#[async_trait]
impl DaemonClient for PipeClient {
    async fn list_outputs(&self) -> Result<Topology, IpcError> {
        self.call("list_outputs", serde_json::json!({})).await
    }

    async fn set_output_enabled(
        &self,
        target: &str,
        enabled: Option<bool>,
        opts: SetOpts,
    ) -> Result<SetOutcome, IpcError> {
        let mode = match enabled {
            Some(true) => "on",
            Some(false) => "off",
            None => "toggle",
        };
        self.call(
            "set_output_enabled",
            serde_json::json!({ "target": target, "mode": mode, "opts": opts }),
        )
        .await
    }

    async fn confirm_pending(&self) -> Result<bool, IpcError> {
        self.call("confirm_pending", serde_json::json!({})).await
    }

    async fn revert_pending(&self) -> Result<bool, IpcError> {
        self.call("revert_pending", serde_json::json!({})).await
    }

    async fn list_profiles(&self) -> Result<ProfilesInfo, IpcError> {
        self.call("list_profiles", serde_json::json!({})).await
    }

    async fn apply_profile(&self, name: &str) -> Result<SetOutcome, IpcError> {
        self.call("apply_profile", serde_json::json!({ "name": name }))
            .await
    }

    async fn save_profile(&self, name: &str) -> Result<(), IpcError> {
        self.call("save_profile", serde_json::json!({ "name": name }))
            .await
    }

    async fn delete_profile(&self, name: &str) -> Result<bool, IpcError> {
        self.call("delete_profile", serde_json::json!({ "name": name }))
            .await
    }

    async fn set_alias(&self, alias: &str, target: &str) -> Result<(), IpcError> {
        self.call(
            "set_alias",
            serde_json::json!({ "alias": alias, "target": target }),
        )
        .await
    }

    async fn get_status(&self) -> Result<StatusInfo, IpcError> {
        self.call("get_status", serde_json::json!({})).await
    }

    async fn quit(&self) -> Result<(), IpcError> {
        self.call("quit", serde_json::json!({})).await
    }
}
