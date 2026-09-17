use crate::config::ManualDevice;
use crate::core::{Core, Snapshot};
use crate::types::*;
use std::sync::Arc;
use tauri::State;
use tracing::info;

type CoreState<'a> = State<'a, Arc<Core>>;

fn transport_cmd(action: &str) -> Option<DeviceCmd> {
    match action {
        "play" => Some(DeviceCmd::Play),
        "pause" => Some(DeviceCmd::Pause),
        "next" => Some(DeviceCmd::Next),
        "prev" => Some(DeviceCmd::Prev),
        _ => None,
    }
}

#[tauri::command]
pub fn get_state(core: CoreState) -> Snapshot {
    core.snapshot()
}

#[tauri::command]
pub fn set_volume(core: CoreState, id: String, level: f32) {
    info!(id=%id, level, "ui: set device volume");
    core.set_device_volume_from_ui(&id, level.clamp(0.0, 1.0));
}

#[tauri::command]
pub fn set_muted(core: CoreState, id: String, muted: bool) {
    info!(id=%id, muted, "ui: set muted");
    core.send_cmd(&id, DeviceCmd::SetMuted(muted));
}

#[tauri::command]
pub fn media_cmd(core: CoreState, id: String, action: String) {
    info!(id=%id, action=%action, "ui: media command");
    if let Some(cmd) = transport_cmd(&action) {
        core.send_cmd(&id, cmd);
    }
}

#[tauri::command]
pub fn rename_device(core: CoreState, id: String, name: Option<String>) {
    info!(id=%id, name=?name, "ui: rename device");
    {
        let mut inner = core.inner.lock().unwrap();
        if let Some(e) = inner.devices.get_mut(&id) {
            e.info.custom_name = name.filter(|n| !n.trim().is_empty());
        }
    }
    core.save_config();
    core.emit_state();
}

#[tauri::command]
pub fn delete_device(core: CoreState, id: String) {
    core.delete_device(&id);
}

#[tauri::command]
pub fn recalibrate_roku(core: CoreState, id: String) {
    info!(id=%id, "ui: recalibrate roku");
    core.send_cmd(&id, DeviceCmd::Refresh);
}

#[tauri::command]
pub fn save_groups(app: tauri::AppHandle, core: CoreState, groups: Vec<AppGroup>) {
    info!(count = groups.len(), "ui: save groups");
    core.inner.lock().unwrap().cfg.groups = groups;
    core.save_config();
    core.emit_state();
    crate::tray::rebuild_tray_menu(&app, &core);
}

#[tauri::command]
pub fn set_group_volume(core: CoreState, group_id: String, level: f32) {
    info!(group=%group_id, level, "ui: set group volume");
    core.set_group_volume(&group_id, level.clamp(0.0, 1.0));
}

#[tauri::command]
pub fn group_media(core: CoreState, group_id: String, action: String) {
    info!(group=%group_id, action=%action, "ui: group media command");
    if let Some(cmd) = transport_cmd(&action) {
        core.group_transport(&group_id, cmd);
    }
}

#[tauri::command]
pub fn save_schedules(core: CoreState, schedules: Vec<ScheduleEvent>) {
    info!(count = schedules.len(), "ui: save schedules");
    core.inner.lock().unwrap().cfg.schedules = schedules;
    core.save_config();
    core.emit_state();
}

#[tauri::command]
pub fn set_settings(app: tauri::AppHandle, core: CoreState, settings: crate::config::Settings) {
    use tauri_plugin_autostart::ManagerExt;
    info!(?settings, "ui: settings changed");
    let autostart = app.autolaunch();
    if settings.start_with_windows {
        let _ = autostart.enable();
    } else {
        let _ = autostart.disable();
    }
    core.inner.lock().unwrap().cfg.settings = settings;
    core.save_config();
    core.emit_state();
}

#[tauri::command]
pub fn export_config(core: CoreState, path: String) -> Result<(), String> {
    info!(path=%path, "ui: export config");
    core.save_config();
    std::fs::copy(crate::config::config_path(), &path)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn import_config(app: tauri::AppHandle, core: CoreState, path: String) -> Result<(), String> {
    info!(path=%path, "ui: import config");
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let cfg: crate::config::AppConfig = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    {
        let mut inner = core.inner.lock().unwrap();
        // Keep live device entries; adopt imported metadata where ids match.
        for imported in &cfg.known_devices {
            if let Some(e) = inner.devices.get_mut(&imported.id) {
                e.info.custom_name = imported.custom_name.clone();
            } else {
                let mut info = imported.clone();
                info.online = false;
                info.media = None;
                inner.devices.insert(info.id.clone(), crate::core::Entry { info, cmd: None });
            }
        }
        inner.cfg.groups = cfg.groups;
        inner.cfg.schedules = cfg.schedules;
        inner.cfg.settings = cfg.settings;
        inner.cfg.manual_devices = cfg.manual_devices;
        inner.cfg.roku_levels = cfg.roku_levels;
        inner.cfg.lg_keys = cfg.lg_keys;
    }
    core.save_config();
    core.spawn_manual_actors();
    core.respawn_known_cast_devices();
    core.emit_state();
    crate::tray::rebuild_tray_menu(&app, &core);
    Ok(())
}

#[tauri::command]
pub fn add_manual_device(core: CoreState, device: ManualDevice) {
    info!(?device, "ui: add manual device");
    {
        let mut inner = core.inner.lock().unwrap();
        inner.cfg.manual_devices.retain(|m| m.ip != device.ip || m.backend != device.backend);
        inner.cfg.manual_devices.push(device.clone());
    }
    core.ensure_manual_device(&device);
    core.save_config();
}

#[tauri::command]
pub async fn scan_roku(core: CoreState<'_>) -> Result<usize, String> {
    info!("ui: roku SSDP scan");
    let found = crate::backends::roku::discover().await;
    let count = found.len();
    for (ip, name, model, _serial) in found {
        let device = ManualDevice {
            backend: Backend::Roku,
            ip,
            port: 8060,
            name: if name.is_empty() { model } else { name },
        };
        {
            let mut inner = core.inner.lock().unwrap();
            if inner.cfg.manual_devices.iter().any(|m| m.ip == device.ip && m.backend == Backend::Roku) {
                continue;
            }
            inner.cfg.manual_devices.push(device.clone());
        }
        core.ensure_manual_device(&device);
    }
    core.save_config();
    Ok(count)
}

#[tauri::command]
pub fn get_log_tail(lines: usize) -> Vec<String> {
    let dir = crate::config::config_dir().join("logs");
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if newest.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                        newest = Some((modified, entry.path()));
                    }
                }
            }
        }
    }
    let Some((_, path)) = newest else { return vec![] };
    let Ok(text) = std::fs::read_to_string(&path) else { return vec![] };
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].iter().map(|s| s.to_string()).collect()
}

#[tauri::command]
pub fn open_log_folder(app: tauri::AppHandle) {
    use tauri_plugin_opener::OpenerExt;
    let dir = crate::config::config_dir().join("logs");
    let _ = app.opener().open_path(dir.to_string_lossy(), None::<&str>);
}
