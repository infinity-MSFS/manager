//! The tray icon. Closing the window hides it to the tray, so aircraft
//! that depend on the manager keep working; Quit from the tray exits, and
//! warns first while a gauge is connected.

use tauri::Manager;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::deep_link::focus_main;

const OPEN: &str = "tray-open";
const QUIT: &str = "tray-quit";

pub(crate) fn install(app: &tauri::AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, OPEN, "Open Infinity Manager", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit_item])?;
    let mut tray = TrayIconBuilder::with_id("infinity-manager")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Infinity Manager")
        .on_menu_event(|app, event| match event.id().as_ref() {
            OPEN => focus_main(app),
            QUIT => quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                focus_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;

    if let Some(window) = app.get_webview_window("main") {
        let hide = window.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = hide.hide();
            }
        });
    }
    Ok(())
}

fn quit(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let bridges = app.state::<infinity_bridge::BridgeManager>();
        let services = &app.state::<crate::state::AppState>().providers.sim_services;
        let in_use = services.iter().any(|s| s.in_use())
            || bridges.statuses().await.iter().any(|s| s.connected);
        if !in_use {
            app.exit(0);
            return;
        }
        let exit = app.clone();
        app.dialog()
            .message(
                "An aircraft is using Infinity Manager. Quitting stops the features it \
                 provides until you open the manager again. Closing the window keeps it \
                 running in the tray.",
            )
            .title("An aircraft is using the manager")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Quit anyway".into(),
                "Keep running".into(),
            ))
            .show(move |confirmed| {
                if confirmed {
                    exit.exit(0);
                }
            });
    });
}
