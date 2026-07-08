//! Transport-independent RPC dispatch: both the DBus service (Linux) and the
//! named-pipe server (Windows) funnel through [`dispatch`], so the daemon API
//! behaves identically on every platform.

use crate::engine::{Engine, EngineError};
use nosignal_ipc::types::{Envelope, SetOpts};
use serde::Serialize;
use std::sync::Arc;

pub fn envelope<T: Serialize>(result: Result<T, EngineError>) -> String {
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

fn bad_request(msg: String) -> String {
    envelope::<()>(Err(EngineError::Store(msg)))
}

/// Execute one API method. `params` field names match the DBus argument
/// names; the returned string is a JSON [`Envelope`].
pub async fn dispatch(
    engine: &Arc<Engine>,
    shutdown: &Arc<tokio::sync::Notify>,
    method: &str,
    params: serde_json::Value,
) -> String {
    let str_param = |key: &str| -> Result<String, String> {
        params
            .get(key)
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| format!("missing string param '{key}'"))
    };

    match method {
        "list_outputs" => envelope(engine.snapshot().await),
        "set_output_enabled" => {
            let (target, mode) = match (str_param("target"), str_param("mode")) {
                (Ok(t), Ok(m)) => (t, m),
                (Err(e), _) | (_, Err(e)) => return bad_request(e),
            };
            let enabled = match mode.as_str() {
                "on" => Some(true),
                "off" => Some(false),
                "toggle" => None,
                other => return bad_request(format!("bad mode '{other}'")),
            };
            let opts: SetOpts = match params.get("opts") {
                Some(v) => match serde_json::from_value(v.clone()) {
                    Ok(o) => o,
                    Err(e) => return bad_request(format!("bad opts: {e}")),
                },
                None => SetOpts::default(),
            };
            envelope(engine.set_enabled(&target, enabled, opts).await)
        }
        "confirm_pending" => envelope(engine.confirm_pending().await),
        "revert_pending" => envelope(engine.revert_pending().await),
        "list_profiles" => envelope(engine.list_profiles().await),
        "apply_profile" => match str_param("name") {
            Ok(name) => envelope(engine.apply_profile(&name).await),
            Err(e) => bad_request(e),
        },
        "save_profile" => match str_param("name") {
            Ok(name) => envelope(engine.save_profile(&name).await),
            Err(e) => bad_request(e),
        },
        "delete_profile" => match str_param("name") {
            Ok(name) => envelope(engine.delete_profile(&name).await),
            Err(e) => bad_request(e),
        },
        "set_alias" => match (str_param("alias"), str_param("target")) {
            (Ok(alias), Ok(target)) => envelope(engine.set_alias(&alias, &target).await),
            (Err(e), _) | (_, Err(e)) => bad_request(e),
        },
        "get_status" => envelope(engine.status().await),
        "quit" => {
            shutdown.notify_waiters();
            envelope(Ok(()))
        }
        other => bad_request(format!("unknown method '{other}'")),
    }
}
