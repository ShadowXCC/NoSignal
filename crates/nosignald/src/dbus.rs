//! DBus service: the Linux IPC surface and the v1 automation hook
//! (`busctl --user call org.nosignal.Daemon1 ...`).
//!
//! Every method returns a JSON [`Envelope`] string so results and typed
//! errors survive the trip identically on every transport.

use crate::engine::{Engine, EngineError};
use nosignal_ipc::types::{Envelope, SetOpts};
use serde::Serialize;
use std::sync::Arc;
use zbus::object_server::SignalEmitter;

fn envelope<T: Serialize>(result: Result<T, EngineError>) -> String {
    let env = match result {
        Ok(v) => Envelope::Ok(v),
        Err(e) => Envelope::Err {
            kind: e.kind(),
            message: e.to_string(),
        },
    };
    serde_json::to_string(&env).unwrap_or_else(|e| {
        format!(r#"{{"status":"err","data":{{"kind":"other","message":"encode: {e}"}}}}"#)
    })
}

pub struct DaemonIface {
    pub engine: Arc<Engine>,
    pub shutdown: Arc<tokio::sync::Notify>,
}

#[zbus::interface(name = "org.nosignal.Daemon1")]
impl DaemonIface {
    async fn list_outputs(&self) -> String {
        envelope(self.engine.snapshot().await)
    }

    /// `mode` is "on" | "off" | "toggle"; `opts_json` is a [`SetOpts`] JSON.
    async fn set_output_enabled(&self, target: String, mode: String, opts_json: String) -> String {
        let enabled = match mode.as_str() {
            "on" => Some(true),
            "off" => Some(false),
            "toggle" => None,
            other => {
                return envelope::<()>(Err(EngineError::Store(format!(
                    "bad mode '{other}' (expected on|off|toggle)"
                ))));
            }
        };
        let opts: SetOpts = match serde_json::from_str(&opts_json) {
            Ok(o) => o,
            Err(e) => {
                return envelope::<()>(Err(EngineError::Store(format!("bad opts: {e}"))));
            }
        };
        envelope(self.engine.set_enabled(&target, enabled, opts).await)
    }

    async fn confirm_pending(&self) -> String {
        envelope(self.engine.confirm_pending().await)
    }

    async fn revert_pending(&self) -> String {
        envelope(self.engine.revert_pending().await)
    }

    async fn list_profiles(&self) -> String {
        envelope(self.engine.list_profiles().await)
    }

    async fn apply_profile(&self, name: String) -> String {
        envelope(self.engine.apply_profile(&name).await)
    }

    async fn save_profile(&self, name: String) -> String {
        envelope(self.engine.save_profile(&name).await)
    }

    async fn delete_profile(&self, name: String) -> String {
        envelope(self.engine.delete_profile(&name).await)
    }

    async fn set_alias(&self, alias: String, target: String) -> String {
        envelope(self.engine.set_alias(&alias, &target).await)
    }

    async fn get_status(&self) -> String {
        envelope(self.engine.status().await)
    }

    async fn quit(&self) -> String {
        self.shutdown.notify_waiters();
        envelope(Ok(()))
    }

    /// Broadcast channel for clients; payload is a JSON
    /// [`nosignal_ipc::types::DaemonEvent`].
    #[zbus(signal)]
    pub async fn daemon_event(
        emitter: &SignalEmitter<'_>,
        payload_json: String,
    ) -> zbus::Result<()>;
}

/// Claim the well-known name and serve the interface. Fails when another
/// daemon instance already owns the name (single-instance per session).
pub async fn serve(
    engine: Arc<Engine>,
    shutdown: Arc<tokio::sync::Notify>,
) -> zbus::Result<zbus::Connection> {
    let iface = DaemonIface {
        engine: engine.clone(),
        shutdown,
    };
    let connection = zbus::connection::Builder::session()?
        .name(nosignal_core::DBUS_NAME)?
        .serve_at(nosignal_core::DBUS_PATH, iface)?
        .build()
        .await?;

    // Forward engine events as DBus signals.
    let mut events = engine.subscribe();
    let conn = connection.clone();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let payload = serde_json::to_string(&event).unwrap_or_default();
                    if let Ok(iface_ref) = conn
                        .object_server()
                        .interface::<_, DaemonIface>(nosignal_core::DBUS_PATH)
                        .await
                    {
                        let _ =
                            DaemonIface::daemon_event(iface_ref.signal_emitter(), payload).await;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    Ok(connection)
}
