use crate::core::{Core, NowPlaying};
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use tracing::info;

pub const TRAY_ID: &str = "main-tray";

/// Sessions beyond this are left to the app window, to keep the menu short.
const MAX_SESSIONS: usize = 3;

fn session_label(n: &NowPlaying) -> String {
    let what = match (&n.title, &n.artist) {
        (Some(t), Some(a)) => format!("{t} – {a}"),
        (Some(t), None) => t.clone(),
        _ => n.app.clone().unwrap_or_else(|| "Unknown track".into()),
    };
    let state = if n.playing { "Playing" } else { "Paused" };
    let label = format!("{state} on {}: {what}", n.device);
    // Win32 menus treat '&' as an accelerator marker.
    let label = label.replace('&', "&&");
    if label.chars().count() > 64 {
        format!("{}…", label.chars().take(63).collect::<String>())
    } else {
        label
    }
}

pub fn build_menu(app: &AppHandle, core: &Core) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;

    let sessions = core.now_playing();
    for n in sessions.iter().take(MAX_SESSIONS) {
        menu.append(&MenuItem::with_id(app, format!("np|{}", n.id), session_label(n), false, None::<&str>)?)?;
        let toggle = if n.playing { "Pause" } else { "Play" };
        for (action, label) in [("toggle", toggle), ("next", "Next track"), ("prev", "Previous track")] {
            menu.append(&MenuItem::with_id(app, format!("mt|{}|{}", n.id, action), label, true, None::<&str>)?)?;
        }
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }

    let groups = core.inner.lock().unwrap().cfg.groups.clone();

    // With a single sync group there's nothing to choose between, so its
    // options sit directly in the first menu instead of behind a submenu.
    if let [g] = groups.as_slice() {
        let header = format!("{} volume", g.name).replace('&', "&&");
        menu.append(&MenuItem::with_id(app, format!("gh|{}", g.id), header, false, None::<&str>)?)?;
        for pct in (5..=100).step_by(5) {
            menu.append(&MenuItem::with_id(app, format!("gv|{}|{}", g.id, pct), format!("{pct}%"), true, None::<&str>)?)?;
        }
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        for (action, label) in [("play", "Play group"), ("pause", "Pause group"), ("next", "Next (group)"), ("prev", "Previous (group)")] {
            menu.append(&MenuItem::with_id(app, format!("gt|{}|{}", g.id, action), label, true, None::<&str>)?)?;
        }
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        menu.append(&MenuItem::with_id(app, "open", "Open App", true, None::<&str>)?)?;
        menu.append(&MenuItem::with_id(app, "exit", "Exit", true, None::<&str>)?)?;
        return Ok(menu);
    }

    for g in &groups {
        let sub = Submenu::new(app, &g.name, true)?;
        for pct in (5..=100).step_by(5) {
            let item = MenuItem::with_id(app, format!("gv|{}|{}", g.id, pct), format!("{pct}%"), true, None::<&str>)?;
            sub.append(&item)?;
        }
        sub.append(&PredefinedMenuItem::separator(app)?)?;
        for (action, label) in [("play", "Play"), ("pause", "Pause"), ("next", "Next"), ("prev", "Previous")] {
            let item = MenuItem::with_id(app, format!("gt|{}|{}", g.id, action), label, true, None::<&str>)?;
            sub.append(&item)?;
        }
        menu.append(&sub)?;
    }
    if !groups.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    menu.append(&MenuItem::with_id(app, "open", "Open App", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "exit", "Exit", true, None::<&str>)?)?;
    Ok(menu)
}

pub fn rebuild_tray_menu(app: &AppHandle, core: &Core) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if let Ok(menu) = build_menu(app, core) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

pub fn setup_tray(app: &AppHandle, core: Arc<Core>) -> tauri::Result<()> {
    let menu = build_menu(app, &core)?;
    let core_for_menu = core.clone();
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Volume Sync")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| {
            let id = event.id().as_ref();
            info!(menu_id=%id, "tray: menu clicked");
            match id {
                "open" => show_main_window(app),
                "exit" => {
                    core_for_menu.save_config();
                    app.exit(0);
                }
                other => {
                    let parts: Vec<&str> = other.split('|').collect();
                    match parts.as_slice() {
                        ["gv", gid, pct] => {
                            if let Ok(pct) = pct.parse::<f32>() {
                                core_for_menu.set_group_volume(gid, pct / 100.0);
                            }
                        }
                        ["mt", id, action] => match *action {
                            "toggle" => core_for_menu.media_toggle(id),
                            "next" => core_for_menu.send_cmd(id, crate::types::DeviceCmd::Next),
                            "prev" => core_for_menu.send_cmd(id, crate::types::DeviceCmd::Prev),
                            _ => {}
                        },
                        ["gt", gid, action] => {
                            let cmd = match *action {
                                "play" => Some(crate::types::DeviceCmd::Play),
                                "pause" => Some(crate::types::DeviceCmd::Pause),
                                "next" => Some(crate::types::DeviceCmd::Next),
                                "prev" => Some(crate::types::DeviceCmd::Prev),
                                _ => None,
                            };
                            if let Some(cmd) = cmd {
                                core_for_menu.group_transport(gid, cmd);
                            }
                        }
                        _ => {}
                    }
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = event {
                show_main_window(tray.app_handle());
            } else if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}
