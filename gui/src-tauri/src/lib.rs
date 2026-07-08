//! NoSignal GUI shell: a thin Tauri client over the daemon IPC.
//!
//! The golden rule applies here too — zero display logic. Commands proxy the
//! daemon API to the Svelte frontend; a background task forwards daemon
//! events to the webview and keeps the tray menu current.

mod tray;

use nosignal_ipc::types::{ProfilesInfo, SetOpts, SetOutcome, StatusInfo};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter, Manager};

pub struct AppState {
    pub close_to_tray: AtomicBool,
}

fn err_str(e: impl std::fmt::Display) -> String {
    e.to_string()
}

async fn client() -> Result<Box<dyn nosignal_ipc::DaemonClient>, String> {
    nosignal_ipc::connect().await.map_err(err_str)
}

#[tauri::command]
async fn outputs() -> Result<nosignal_core::Topology, String> {
    client().await?.list_outputs().await.map_err(err_str)
}

#[tauri::command]
async fn set_output(
    app: AppHandle,
    target: String,
    mode: String,
    force: bool,
    revert_secs: Option<u64>,
) -> Result<SetOutcome, String> {
    let enabled = match mode.as_str() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    };
    let outcome = client()
        .await?
        .set_output_enabled(
            &target,
            enabled,
            SetOpts {
                force,
                no_timer: false,
                revert_secs,
            },
        )
        .await
        .map_err(err_str);
    tray::schedule_rebuild(&app);
    outcome
}

#[tauri::command]
async fn confirm_pending() -> Result<bool, String> {
    client().await?.confirm_pending().await.map_err(err_str)
}

#[tauri::command]
async fn revert_pending() -> Result<bool, String> {
    client().await?.revert_pending().await.map_err(err_str)
}

#[tauri::command]
async fn profiles() -> Result<ProfilesInfo, String> {
    client().await?.list_profiles().await.map_err(err_str)
}

#[tauri::command]
async fn profile_apply(app: AppHandle, name: String) -> Result<SetOutcome, String> {
    let result = client().await?.apply_profile(&name).await.map_err(err_str);
    tray::schedule_rebuild(&app);
    result
}

#[tauri::command]
async fn profile_save(app: AppHandle, name: String) -> Result<(), String> {
    let result = client().await?.save_profile(&name).await.map_err(err_str);
    tray::schedule_rebuild(&app);
    result
}

#[tauri::command]
async fn profile_delete(app: AppHandle, name: String) -> Result<bool, String> {
    let result = client().await?.delete_profile(&name).await.map_err(err_str);
    tray::schedule_rebuild(&app);
    result
}

#[tauri::command]
async fn set_alias(alias: String, target: String) -> Result<(), String> {
    client()
        .await?
        .set_alias(&alias, &target)
        .await
        .map_err(err_str)
}

#[tauri::command]
async fn status() -> Result<StatusInfo, String> {
    client().await?.get_status().await.map_err(err_str)
}

#[tauri::command]
fn set_close_to_tray(state: tauri::State<'_, AppState>, enabled: bool) {
    state.close_to_tray.store(enabled, Ordering::Relaxed);
}

/// Best-effort daemon start when it isn't reachable: systemd unit first
/// (Linux), then a `nosignald` binary next to this executable or on PATH.
#[tauri::command]
async fn daemon_start() -> Result<(), String> {
    if let Ok(c) = client().await
        && c.get_status().await.is_ok()
    {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = tokio::process::Command::new("systemctl")
            .args(["--user", "start", "nosignal-daemon"])
            .status()
            .await
            && status.success()
        {
            return Ok(());
        }
    }
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(daemon_bin_name())))
        .filter(|p| p.exists());
    let bin = sibling.unwrap_or_else(|| daemon_bin_name().into());
    std::process::Command::new(bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start nosignald: {e}"))?;
    Ok(())
}

fn daemon_bin_name() -> &'static str {
    if cfg!(windows) { "nosignald.exe" } else { "nosignald" }
}

/// Forward daemon events to the webview and refresh the tray. Reconnects
/// forever — the daemon may come and go.
fn spawn_event_forwarder(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        use futures::StreamExt;
        loop {
            let Ok(c) = client().await else {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            };
            let Ok(mut events) = c.events().await else {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            };
            let _ = app.emit("daemon-connected", ());
            tray::schedule_rebuild(&app);
            while let Some(event) = events.next().await {
                let _ = app.emit("daemon-event", &event);
                tray::schedule_rebuild(&app);
            }
            let _ = app.emit("daemon-disconnected", ());
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    });
}

/// Register profile hotkeys with the OS (Windows only — on Linux the daemon
/// owns hotkeys via the GlobalShortcuts portal).
#[cfg(target_os = "windows")]
fn register_windows_hotkeys(app: &AppHandle) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let Ok(c) = client().await else { return };
        let Ok(info) = c.list_profiles().await else {
            return;
        };
        for profile in info.profiles {
            let Some(hotkey) = profile.hotkey else {
                continue;
            };
            let name = profile.name.clone();
            let gs = handle.global_shortcut();
            let handle2 = handle.clone();
            if let Err(e) = gs.on_shortcut(hotkey.as_str(), move |_app, _shortcut, _event| {
                let name = name.clone();
                let handle3 = handle2.clone();
                tauri::async_runtime::spawn(async move {
                    if let Ok(c) = client().await {
                        let _ = c.apply_profile(&name).await;
                        tray::schedule_rebuild(&handle3);
                    }
                });
            }) {
                eprintln!("could not register hotkey '{hotkey}': {e}");
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ));

    #[cfg(target_os = "windows")]
    let builder = builder.plugin(tauri_plugin_global_shortcut::Builder::new().build());

    builder
        .manage(AppState {
            close_to_tray: AtomicBool::new(true),
        })
        .invoke_handler(tauri::generate_handler![
            outputs,
            set_output,
            confirm_pending,
            revert_pending,
            profiles,
            profile_apply,
            profile_save,
            profile_delete,
            set_alias,
            status,
            set_close_to_tray,
            daemon_start,
        ])
        .setup(|app| {
            tray::init(app.handle())?;
            spawn_event_forwarder(app.handle().clone());
            #[cfg(target_os = "windows")]
            register_windows_hotkeys(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                if state.close_to_tray.load(Ordering::Relaxed) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
