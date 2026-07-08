//! Tray icon: per-output toggles, profile quick-switch, open, quit.
//!
//! The menu is rebuilt whenever daemon state changes. On Linux the tray needs
//! an SNI host (AppIndicator extension on stock GNOME); when absent, tray
//! creation fails silently and the window remains the primary surface.

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

const TRAY_ID: &str = "nosignal-tray";
static REBUILDING: AtomicBool = AtomicBool::new(false);

pub fn init(app: &AppHandle) -> tauri::Result<()> {
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "open", "Open NoSignal", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?,
        ],
    )?;

    let build_result = TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().cloned().unwrap_or_else(|| {
            tauri::image::Image::new_owned(vec![0, 0, 0, 255], 1, 1)
        }))
        .tooltip("NoSignal")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| on_menu_event(app.clone(), event.id().as_ref().to_string()))
        .build(app);

    match build_result {
        Ok(_) => schedule_rebuild(app),
        // No SNI host / no tray support: degrade silently (documented).
        Err(e) => eprintln!("tray unavailable: {e}"),
    }
    Ok(())
}

/// Debounced tray refresh from any thread.
pub fn schedule_rebuild(app: &AppHandle) {
    if REBUILDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        REBUILDING.store(false, Ordering::SeqCst);
        if let Err(e) = rebuild(&app).await {
            eprintln!("tray rebuild failed: {e}");
        }
    });
}

async fn rebuild(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return Ok(());
    };
    let client = match nosignal_ipc::connect().await {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    let topology = client.list_outputs().await;
    let profiles = client.list_profiles().await;

    let menu = Menu::new(app)?;
    if let Ok(topology) = topology {
        for output in &topology.outputs {
            let label = match &output.alias {
                Some(alias) => format!("{alias} ({})", output.identity.connector),
                None => output.identity.connector.clone(),
            };
            menu.append(&CheckMenuItem::with_id(
                app,
                format!("output:{}", output.identity.connector),
                label,
                true,
                output.enabled,
                None::<&str>,
            )?)?;
        }
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    if let Ok(info) = profiles {
        for profile in &info.profiles {
            let label = if profile.active {
                format!("● {}", profile.name)
            } else {
                format!("  {}", profile.name)
            };
            menu.append(&MenuItem::with_id(
                app,
                format!("profile:{}", profile.name),
                label,
                true,
                None::<&str>,
            )?)?;
        }
        if !info.profiles.is_empty() {
            menu.append(&PredefinedMenuItem::separator(app)?)?;
        }
    }
    menu.append(&MenuItem::with_id(
        app,
        "open",
        "Open NoSignal",
        true,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?)?;
    tray.set_menu(Some(menu))?;
    Ok(())
}

fn on_menu_event(app: AppHandle, id: String) {
    match id.as_str() {
        "open" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
        "quit" => app.exit(0),
        other => {
            let action = other.to_string();
            let app2 = app.clone();
            tauri::async_runtime::spawn(async move {
                let Ok(client) = nosignal_ipc::connect().await else {
                    return;
                };
                if let Some(connector) = action.strip_prefix("output:") {
                    // Tray toggles acknowledge risk; mandatory revert timers
                    // still arm in the daemon, which is the safety net here.
                    let _ = client
                        .set_output_enabled(
                            connector,
                            None,
                            nosignal_ipc::types::SetOpts {
                                force: true,
                                no_timer: false,
                                revert_secs: None,
                            },
                        )
                        .await;
                } else if let Some(name) = action.strip_prefix("profile:") {
                    let _ = client.apply_profile(name).await;
                }
                crate::tray::schedule_rebuild(&app2);
            });
        }
    }
}
