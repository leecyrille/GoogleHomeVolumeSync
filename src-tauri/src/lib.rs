mod backends;
mod cast;
mod commands;
mod config;
mod core;
mod media_server;
mod sync_play;
mod tray;
mod types;

use crate::core::Core;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tauri::Manager;
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // File + console logging. Keep the guard alive for the process lifetime.
    let log_dir = config::config_dir().join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(&log_dir, "volume-sync.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);
    Box::leak(Box::new(guard));
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("info,ghvs_lib=debug"))
        .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(file_writer))
        .with(tracing_subscriber::fmt::layer())
        .init();
    info!("=== Unofficial Google Home Volume Sync starting ===");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main_window(app);
        }))
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::set_volume,
            commands::set_muted,
            commands::media_cmd,
            commands::rename_device,
            commands::set_sync_gain,
            commands::delete_device,
            commands::set_power,
            commands::set_input,
            commands::device_key,
            commands::seek,
            commands::roku_dev_settings,
            commands::roku_install_player,
            commands::play_files,
            commands::play_url,
            commands::play_synced,
            commands::stop_sync,
            commands::recalibrate_roku,
            commands::save_groups,
            commands::set_group_volume,
            commands::group_media,
            commands::save_schedules,
            commands::set_settings,
            commands::export_config,
            commands::import_config,
            commands::add_manual_device,
            commands::scan_roku,
            commands::get_log_tail,
            commands::open_log_folder,
            commands::open_notices,
        ])
        .setup(|app| {
            let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<types::CoreEvent>(256);
            let (lg_key_tx, mut lg_key_rx) = tokio::sync::mpsc::channel::<(String, String)>(8);
            let core = Arc::new(Core::new(app.handle().clone(), event_tx, lg_key_tx));
            app.manage(core.clone());

            // Backend actor events -> core.
            let c = core.clone();
            tauri::async_runtime::spawn(async move {
                while let Some(ev) = event_rx.recv().await {
                    c.handle_event(ev);
                }
            });

            // LG pairing keys -> config.
            let c = core.clone();
            tauri::async_runtime::spawn(async move {
                while let Some((id, key)) = lg_key_rx.recv().await {
                    c.inner.lock().unwrap().cfg.lg_keys.insert(id, key);
                    c.save_config();
                }
            });

            // Continuous mDNS cast discovery.
            let (disc_tx, mut disc_rx) = tokio::sync::mpsc::channel(64);
            cast::discovery::start_cast_browse(disc_tx);
            let c = core.clone();
            tauri::async_runtime::spawn(async move {
                while let Some(d) = disc_rx.recv().await {
                    c.on_cast_discovered(d);
                }
            });

            // Roku volume-level cache persistence + scheduler + periodic config save.
            let c = core.clone();
            tauri::async_runtime::spawn(async move {
                let mut last_fired: HashMap<String, String> = HashMap::new();
                let mut tick = tokio::time::interval(Duration::from_secs(15));
                let mut save_counter = 0u32;
                loop {
                    tick.tick().await;
                    c.run_due_schedules(&mut last_fired);
                    save_counter += 1;
                    if save_counter % 8 == 0 && c.inner.lock().unwrap().cfg_dirty {
                        c.save_config();
                    }
                }
            });

            // Persist Roku cached levels whenever a roku volume event lands:
            // handled via config save cadence above (levels live in actor + events).

            // Reconnect actors for devices we knew about (cast devices before mdns finds them again).
            core.respawn_known_cast_devices();
            core.spawn_manual_actors();

            tray::setup_tray(app.handle(), core.clone())?;

            // Apply the start-with-windows setting (defaults to enabled).
            {
                use tauri_plugin_autostart::ManagerExt;
                let autostart = app.autolaunch();
                let want = core.inner.lock().unwrap().cfg.settings.start_with_windows;
                let _ = if want { autostart.enable() } else { autostart.disable() };
                info!(start_with_windows = want, "autostart: applied");
            }

            // Start hidden if launched with --hidden (autostart).
            if std::env::args().any(|a| a == "--hidden") {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.hide();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Hide to tray instead of exiting.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
