//! Linux client transport: DBus session bus.
//!
//! The daemon is DBus-activatable, so simply calling a method starts it when
//! the service file is installed. Every method carries JSON payloads inside
//! an [`Envelope`] so error kinds survive the trip.

use crate::types::*;
use crate::{DaemonClient, IpcError};
use async_trait::async_trait;
use nosignal_core::Topology;
use serde::de::DeserializeOwned;
use zbus::{Connection, proxy};

// Interface/name/path literals must match nosignal_core::{DBUS_NAME, DBUS_PATH}
// (asserted in tests below; proxy attributes need literals).
#[proxy(
    interface = "io.github.shadowxcc.NoSignal.Daemon1",
    default_service = "io.github.shadowxcc.NoSignal.Daemon1",
    default_path = "/io/github/shadowxcc/NoSignal/Daemon1"
)]
trait Daemon {
    fn list_outputs(&self) -> zbus::Result<String>;
    fn set_output_enabled(
        &self,
        target: String,
        mode: String,
        opts_json: String,
    ) -> zbus::Result<String>;
    fn confirm_pending(&self) -> zbus::Result<String>;
    fn revert_pending(&self) -> zbus::Result<String>;
    fn list_profiles(&self) -> zbus::Result<String>;
    fn apply_profile(&self, name: String) -> zbus::Result<String>;
    fn save_profile(&self, name: String) -> zbus::Result<String>;
    fn delete_profile(&self, name: String) -> zbus::Result<String>;
    fn set_alias(&self, alias: String, target: String) -> zbus::Result<String>;
    fn get_status(&self) -> zbus::Result<String>;
    fn quit(&self) -> zbus::Result<String>;

    #[zbus(signal)]
    fn daemon_event(&self, payload_json: String) -> zbus::Result<()>;
}

pub struct DbusClient {
    proxy: DaemonProxy<'static>,
}

impl DbusClient {
    pub async fn connect() -> Result<Self, IpcError> {
        let connection = Connection::session()
            .await
            .map_err(|e| IpcError::Unreachable(format!("no session bus: {e}")))?;
        let proxy = DaemonProxy::new(&connection)
            .await
            .map_err(|e| IpcError::Unreachable(e.to_string()))?;
        Ok(Self { proxy })
    }
}

fn decode<T: DeserializeOwned>(raw: zbus::Result<String>) -> Result<T, IpcError> {
    let text = raw.map_err(map_zbus_err)?;
    let envelope: Envelope<T> =
        serde_json::from_str(&text).map_err(|e| IpcError::Protocol(e.to_string()))?;
    envelope.into_result()
}

fn map_zbus_err(e: zbus::Error) -> IpcError {
    match &e {
        zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" =>
        {
            IpcError::Unreachable("daemon is not running and not activatable".into())
        }
        _ => IpcError::Unreachable(e.to_string()),
    }
}

#[async_trait]
impl DaemonClient for DbusClient {
    /// Note: the zbus proxy macro generates a `DaemonEvent` args struct for
    /// the signal, so the wire type is referenced by full path here.
    async fn events(
        &self,
    ) -> Result<futures::stream::BoxStream<'static, crate::types::DaemonEvent>, IpcError> {
        use futures::StreamExt;
        let stream = self
            .proxy
            .receive_daemon_event()
            .await
            .map_err(|e| IpcError::Unreachable(e.to_string()))?;
        Ok(stream
            .filter_map(|signal| async move {
                let args = signal.args().ok()?;
                serde_json::from_str::<crate::types::DaemonEvent>(&args.payload_json).ok()
            })
            .boxed())
    }

    async fn list_outputs(&self) -> Result<Topology, IpcError> {
        decode(self.proxy.list_outputs().await)
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
        let opts_json =
            serde_json::to_string(&opts).map_err(|e| IpcError::Protocol(e.to_string()))?;
        decode(
            self.proxy
                .set_output_enabled(target.to_string(), mode.to_string(), opts_json)
                .await,
        )
    }

    async fn confirm_pending(&self) -> Result<bool, IpcError> {
        decode(self.proxy.confirm_pending().await)
    }

    async fn revert_pending(&self) -> Result<bool, IpcError> {
        decode(self.proxy.revert_pending().await)
    }

    async fn list_profiles(&self) -> Result<ProfilesInfo, IpcError> {
        decode(self.proxy.list_profiles().await)
    }

    async fn apply_profile(&self, name: &str) -> Result<SetOutcome, IpcError> {
        decode(self.proxy.apply_profile(name.to_string()).await)
    }

    async fn save_profile(&self, name: &str) -> Result<(), IpcError> {
        decode(self.proxy.save_profile(name.to_string()).await)
    }

    async fn delete_profile(&self, name: &str) -> Result<bool, IpcError> {
        decode(self.proxy.delete_profile(name.to_string()).await)
    }

    async fn set_alias(&self, alias: &str, target: &str) -> Result<(), IpcError> {
        decode(
            self.proxy
                .set_alias(alias.to_string(), target.to_string())
                .await,
        )
    }

    async fn get_status(&self) -> Result<StatusInfo, IpcError> {
        decode(self.proxy.get_status().await)
    }

    async fn quit(&self) -> Result<(), IpcError> {
        decode(self.proxy.quit().await)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn proxy_literals_match_core_constants() {
        assert_eq!(
            nosignal_core::DBUS_NAME,
            "io.github.shadowxcc.NoSignal.Daemon1"
        );
        assert_eq!(
            nosignal_core::DBUS_PATH,
            "/io/github/shadowxcc/NoSignal/Daemon1"
        );
    }
}
