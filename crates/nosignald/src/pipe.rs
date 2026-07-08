//! Windows IPC transport: a named-pipe server speaking newline-delimited
//! JSON. Requests are `{"id": N, "method": "...", "params": {...}}`, answered
//! with `{"id": N, "payload": <envelope>}`. A `subscribe` request switches
//! the connection into an event stream of `{"event": <DaemonEvent>}` lines.

use crate::engine::Engine;
use crate::rpc;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

/// Claim the pipe name (single-instance check) and start the accept loop.
pub fn start(
    engine: Arc<Engine>,
    shutdown: Arc<tokio::sync::Notify>,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    // Creating the first instance fails when another daemon owns the name.
    let first = ServerOptions::new()
        .first_pipe_instance(true)
        .create(nosignal_core::PIPE_NAME)?;
    Ok(tokio::spawn(accept_loop(first, engine, shutdown)))
}

async fn accept_loop(
    mut server: NamedPipeServer,
    engine: Arc<Engine>,
    shutdown: Arc<tokio::sync::Notify>,
) {
    loop {
        if server.connect().await.is_err() {
            continue;
        }
        let connected = server;
        server = match ServerOptions::new().create(nosignal_core::PIPE_NAME) {
            Ok(next) => next,
            Err(e) => {
                tracing::error!("cannot create next pipe instance: {e}");
                return;
            }
        };
        tokio::spawn(handle_client(connected, engine.clone(), shutdown.clone()));
    }
}

async fn handle_client(
    pipe: NamedPipeServer,
    engine: Arc<Engine>,
    shutdown: Arc<tokio::sync::Notify>,
) {
    let (read, mut write) = tokio::io::split(pipe);
    let mut lines = BufReader::new(read).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let request: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let method = request
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();

        if method == "subscribe" {
            let ack = format!("{{\"id\":{id},\"payload\":{}}}\n", rpc::envelope(Ok(())));
            if write.write_all(ack.as_bytes()).await.is_err() {
                return;
            }
            let mut events = engine.subscribe();
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let Ok(payload) = serde_json::to_string(&event) else {
                            continue;
                        };
                        let line = format!("{{\"event\":{payload}}}\n");
                        if write.write_all(line.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        }

        let params = request
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Object(Default::default()));
        let payload = rpc::dispatch(&engine, &shutdown, &method, params).await;
        let response = format!("{{\"id\":{id},\"payload\":{payload}}}\n");
        if write.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}
