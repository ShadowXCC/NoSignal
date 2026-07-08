//! Global hotkeys on Linux via the XDG GlobalShortcuts portal (GNOME, KDE;
//! wlroots compositors mostly lack this portal — their users bind the CLI in
//! compositor config, and `nosignal hotkeys install` writes GNOME custom
//! keybindings as a portal-free fallback).
//!
//! Each profile with a `hotkey` gets bound as shortcut id `profile-<name>`;
//! activation applies the profile. Binding happens once at daemon startup —
//! after adding/changing hotkeys, restart the daemon (documented).

use crate::engine::Engine;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures::StreamExt;
use std::sync::Arc;

pub async fn run(engine: Arc<Engine>) {
    let shortcuts: Vec<(String, String)> = match engine.list_profiles().await {
        Ok(info) => info
            .profiles
            .iter()
            .filter_map(|p| p.hotkey.clone().map(|h| (p.name.clone(), h)))
            .collect(),
        Err(_) => return,
    };
    if shortcuts.is_empty() {
        tracing::debug!("no profile hotkeys configured");
        return;
    }
    if let Err(e) = bind_and_listen(&engine, &shortcuts).await {
        tracing::info!(
            "GlobalShortcuts portal unavailable ({e}); bind hotkeys in your \
             desktop settings or run `nosignal hotkeys install` on GNOME"
        );
    }
}

async fn bind_and_listen(
    engine: &Arc<Engine>,
    shortcuts: &[(String, String)],
) -> ashpd::Result<()> {
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session(Default::default()).await?;

    let new_shortcuts: Vec<NewShortcut> = shortcuts
        .iter()
        .map(|(name, trigger)| {
            NewShortcut::new(
                format!("profile-{name}"),
                format!("Apply NoSignal profile '{name}'"),
            )
            .preferred_trigger(Some(trigger.as_str()))
        })
        .collect();
    portal
        .bind_shortcuts(&session, &new_shortcuts, None, Default::default())
        .await?
        .response()?;
    tracing::info!("bound {} profile hotkey(s) via portal", shortcuts.len());

    let mut activations = portal.receive_activated().await?;
    while let Some(activation) = activations.next().await {
        let id = activation.shortcut_id().to_string();
        if let Some(name) = id.strip_prefix("profile-") {
            tracing::info!("hotkey: applying profile '{name}'");
            if let Err(e) = engine.apply_profile(name).await {
                tracing::warn!("hotkey apply failed: {e}");
            }
        }
    }
    Ok(())
}
