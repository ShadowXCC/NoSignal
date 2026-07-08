//! `nosignald` — the NoSignal daemon binary.

use nosignald::audio::AudioController;
use nosignald::engine::{Engine, EnginePaths};
use nosignald::select;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let backend = match select::detect().await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("nosignald: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!("display backend: {}", backend.name());

    let audio = detect_audio();
    let engine = Engine::new(backend, audio, EnginePaths::default_locations());
    let shutdown = Arc::new(tokio::sync::Notify::new());

    #[cfg(target_os = "linux")]
    let _connection = match nosignald::dbus::serve(engine.clone(), shutdown.clone()).await {
        Ok(c) => Some(c),
        Err(zbus::Error::NameTaken) => {
            eprintln!("nosignald: another instance already owns {}", {
                nosignal_core::DBUS_NAME
            });
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("nosignald: cannot serve DBus: {e}");
            std::process::exit(1);
        }
    };

    let run = tokio::spawn(engine.clone().run());

    tokio::select! {
        _ = shutdown.notified() => tracing::info!("quit requested"),
        _ = tokio::signal::ctrl_c() => tracing::info!("interrupted"),
        result = run => match result {
            Ok(Ok(())) => tracing::info!("event stream ended"),
            Ok(Err(e)) => {
                tracing::error!("engine stopped: {e}");
                std::process::exit(1);
            }
            Err(e) => {
                tracing::error!("engine task panicked: {e}");
                std::process::exit(1);
            }
        },
    }
}

fn detect_audio() -> Arc<dyn AudioController> {
    #[cfg(target_os = "linux")]
    {
        if let Some(pactl) = nosignald::audio::PactlAudio::detect() {
            tracing::info!("audio: pactl");
            return Arc::new(pactl);
        }
        tracing::info!("audio: pactl not found; audio follow-through disabled");
    }
    Arc::new(nosignald::audio::NoopAudio)
}
