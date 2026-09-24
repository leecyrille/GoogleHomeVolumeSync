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
pub fn set_sync_gain(core: CoreState, id: String, gain: f32) {
    core.set_sync_gain(&id, gain);
}

#[tauri::command]
pub fn set_power(core: CoreState, id: String, on: bool) {
    info!(id=%id, on, "ui: set power");
    core.send_cmd(&id, DeviceCmd::Power(on));
}

#[tauri::command]
pub fn set_input(core: CoreState, id: String, input: String) {
    info!(id=%id, input=%input, "ui: set input");
    core.send_cmd(&id, DeviceCmd::Input(input));
}

#[tauri::command]
pub fn seek(core: CoreState, id: String, position_ms: u64) {
    info!(id=%id, position_ms, "ui: seek");
    core.send_cmd(&id, DeviceCmd::Seek(position_ms));
}

fn device_ip(core: &Core, id: &str) -> Result<String, String> {
    core.inner.lock().unwrap().devices.get(id).map(|e| e.info.ip.clone()).ok_or_else(|| "Unknown device.".to_string())
}

#[tauri::command]
pub async fn roku_dev_settings(core: CoreState<'_>, id: String) -> Result<(), String> {
    info!(id=%id, "ui: open roku developer settings");
    let ip = device_ip(&core, &id)?;
    crate::backends::roku_player::open_dev_settings(&ip).await
}

#[tauri::command]
pub async fn roku_install_player(core: CoreState<'_>, id: String, password: String) -> Result<(), String> {
    info!(id=%id, "ui: install roku player channel");
    let ip = device_ip(&core, &id)?;
    crate::backends::roku_player::install(&ip, &password).await?;
    core.inner.lock().unwrap().cfg.roku_dev_passwords.insert(id.clone(), password);
    core.save_config();
    core.send_cmd(&id, DeviceCmd::Resync);
    Ok(())
}

/// The TV's address, turning it on first if it's off.
async fn tv_ready(core: &Core, id: &str) -> Result<String, String> {
    let (ip, off) = {
        let inner = core.inner.lock().unwrap();
        let e = inner.devices.get(id).ok_or("Unknown device.")?;
        (e.info.ip.clone(), e.info.tv.as_ref().and_then(|t| t.power) == Some(false))
    };
    if off {
        core.send_cmd(id, DeviceCmd::Power(true));
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    }
    Ok(ip)
}

/// Files a Google Cast device's Default Media Receiver can play.
const CAST_EXTS: &[&str] = &["mp4", "m4v", "webm", "mkv", "mov", "mp3", "m4a", "aac", "flac", "wav", "ogg", "opus", "jpg", "jpeg", "png", "gif", "bmp", "webp"];

fn device_backend(core: &Core, id: &str) -> Result<(Backend, String), String> {
    let inner = core.inner.lock().unwrap();
    let e = inner.devices.get(id).ok_or("Unknown device.")?;
    Ok((e.info.backend, e.info.ip.clone()))
}

/// Play files on a Google Cast device: shared from this PC, queued in order.
async fn cast_files(core: &Core, id: &str, ip: &str, paths: &[std::path::PathBuf]) -> Result<(), String> {
    let unsupported: Vec<String> = paths.iter()
        .filter(|p| !p.extension().and_then(|e| e.to_str()).map(|e| CAST_EXTS.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false))
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from)).collect();
    if !unsupported.is_empty() {
        return Err(format!("Cast devices play MP4, WebM, MKV and MOV video, MP3, M4A, AAC, FLAC, WAV and OGG audio, and JPG, PNG, GIF, BMP and WebP pictures. Not supported: {}", unsupported.join(", ")));
    }
    let pictures = paths.iter().filter(|p| crate::media_server::content_type(p).starts_with("image/")).count();
    if pictures > 0 && pictures < paths.len() {
        return Err("Choose either pictures or videos and music, not both at once.".into());
    }
    let mut items = Vec::new();
    for p in paths {
        let content_type = crate::media_server::content_type(p).to_string();
        let subtitles = if content_type.starts_with("video/") {
            match crate::backends::roku_player::sidecar_subtitles(p).and_then(|s| crate::media_server::subtitles_as_vtt(&s)) {
                Some(vtt) => Some(crate::media_server::share(&vtt, ip).await?),
                None => None,
            }
        } else {
            None
        };
        items.push(crate::types::CastItem {
            url: crate::media_server::share(p, ip).await?,
            title: p.file_stem().and_then(|s| s.to_str()).unwrap_or("Media").to_string(),
            content_type,
            subtitles,
            autoplay: true,
        });
    }
    core.send_cmd(id, DeviceCmd::Cast(items));
    Ok(())
}

/// Play one or more files from this PC, in order, with subtitles found next to them.
#[tauri::command]
pub async fn play_files(core: CoreState<'_>, id: String, paths: Vec<String>) -> Result<(), String> {
    use crate::backends::roku_player::{sidecar_subtitles, stream_format, Item};
    info!(id=%id, count = paths.len(), "ui: play files");
    let paths: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
    let (backend, cast_ip) = device_backend(&core, &id)?;
    if backend == Backend::Cast {
        return cast_files(&core, &id, &cast_ip, &paths).await;
    }
    let unsupported: Vec<String> = paths.iter().filter(|p| stream_format(p).is_none())
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from)).collect();
    if !unsupported.is_empty() {
        return Err(format!("Roku TVs play MP4, MOV, MKV and TS video, MP3, M4A, AAC, FLAC and WAV music, and JPG, PNG, GIF and BMP pictures. Not supported: {}", unsupported.join(", ")));
    }
    let kinds: std::collections::BTreeSet<&str> = paths.iter()
        .filter_map(|p| stream_format(p)).map(crate::backends::roku_player::kind).collect();
    if kinds.len() > 1 {
        return Err("On a Roku, choose one kind at a time: videos, music or pictures.".into());
    }
    let pictures = if kinds.contains("image") { paths.len() } else { 0 };
    let ip = tv_ready(&core, &id).await?;
    // Keep the player channel current (e.g. pictures need a newer version).
    if crate::backends::roku_player::needs_upgrade(&ip).await {
        let password = core.inner.lock().unwrap().cfg.roku_dev_passwords.get(&id).cloned();
        match password {
            Some(pw) => {
                info!(id=%id, "roku player: upgrading channel");
                crate::backends::roku_player::install(&ip, &pw).await?;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            None if pictures > 0 => return Err("Showing pictures needs a newer player on this TV. Run Set up video playback again.".into()),
            None => {}
        }
    }
    let mut items = Vec::new();
    for p in &paths {
        let subtitles = match sidecar_subtitles(p) {
            Some(s) => Some(crate::media_server::share(&s, &ip).await?),
            None => None,
        };
        items.push(Item {
            url: crate::media_server::share(p, &ip).await?,
            title: p.file_stem().and_then(|s| s.to_str()).unwrap_or("Video").to_string(),
            fmt: stream_format(p).unwrap_or("mp4"),
            subtitles,
            autoplay: true,
        });
    }
    crate::backends::roku_player::play(&ip, &items).await?;
    core.send_cmd(&id, DeviceCmd::Resync);
    Ok(())
}

static SYNC_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Play one file on several devices at once and keep them in step.
#[tauri::command]
pub async fn play_synced(core: CoreState<'_>, ids: Vec<String>, path: String) -> Result<(), String> {
    use crate::backends::roku_player::{sidecar_subtitles, stream_format, Item};
    let path = std::path::PathBuf::from(path);
    info!(ids=?ids, file=%path.display(), "ui: play in sync");
    if ids.len() < 2 {
        return Err("Pick at least two devices to play in sync.".into());
    }
    let content_type = crate::media_server::content_type(&path).to_string();
    if !(content_type.starts_with("video/") || content_type.starts_with("audio/")) {
        return Err("Synced playback works with a video or music file.".into());
    }
    // Stop any earlier session first.
    core.inner.lock().unwrap().sync = None;

    for id in &ids {
        let (backend, ip) = device_backend(&core, id)?;
        match backend {
            Backend::Cast => {
                let subtitles = if content_type.starts_with("video/") {
                    match sidecar_subtitles(&path).and_then(|s| crate::media_server::subtitles_as_vtt(&s)) {
                        Some(vtt) => Some(crate::media_server::share(&vtt, &ip).await?),
                        None => None,
                    }
                } else {
                    None
                };
                let item = crate::types::CastItem {
                    url: crate::media_server::share(&path, &ip).await?,
                    title: path.file_stem().and_then(|s| s.to_str()).unwrap_or("Media").to_string(),
                    content_type: content_type.clone(),
                    subtitles,
                    autoplay: false,
                };
                core.send_cmd(id, DeviceCmd::Cast(vec![item]));
            }
            Backend::Roku => {
                let fmt = stream_format(&path).ok_or("Roku TVs can't play this file.")?;
                let ip = tv_ready(&core, id).await?;
                if crate::backends::roku_player::needs_upgrade(&ip).await {
                    let pw = core.inner.lock().unwrap().cfg.roku_dev_passwords.get(id).cloned()
                        .ok_or("A Roku TV needs its player updated. Run Set up video playback on it again.")?;
                    crate::backends::roku_player::install(&ip, &pw).await?;
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                let subtitles = match sidecar_subtitles(&path) {
                    Some(s) => Some(crate::media_server::share(&s, &ip).await?),
                    None => None,
                };
                let item = Item {
                    url: crate::media_server::share(&path, &ip).await?,
                    title: path.file_stem().and_then(|s| s.to_str()).unwrap_or("Video").to_string(),
                    fmt,
                    subtitles,
                    autoplay: false,
                };
                crate::backends::roku_player::play(&ip, &[item]).await?;
            }
            _ => return Err("Synced playback works with Google Cast devices and Roku TVs.".into()),
        }
    }

    let session_id = SYNC_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    core.inner.lock().unwrap().sync = Some(crate::sync_play::SyncSession {
        id: session_id,
        members: ids,
        paused: false,
        spread_ms: None,
        status: "Loading…".into(),
    });
    core.emit_state();
    tauri::async_runtime::spawn(crate::sync_play::run((*core).clone(), session_id));
    Ok(())
}

/// Stop keeping devices in step (playback itself carries on).
#[tauri::command]
pub fn stop_sync(core: CoreState) {
    info!("ui: stop sync");
    core.inner.lock().unwrap().sync = None;
    core.emit_state();
}

/// Play a video link (MP4, MKV, TS or an M3U8 live stream) straight from the internet.
#[tauri::command]
pub async fn play_url(core: CoreState<'_>, id: String, url: String) -> Result<(), String> {
    use crate::backends::roku_player::{stream_format_for_url, Item};
    let url = url.trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("That doesn't look like a web link (it should start with http:// or https://).".into());
    }
    info!(id=%id, %url, "ui: play link");
    let (backend, _) = device_backend(&core, &id)?;
    if backend == Backend::Cast {
        let lower = url.split(['?', '#']).next().unwrap_or(&url).to_ascii_lowercase();
        let content_type = if lower.ends_with(".m3u8") { "application/x-mpegURL" }
            else if lower.ends_with(".mp3") { "audio/mpeg" }
            else if lower.ends_with(".webm") { "video/webm" }
            else { "video/mp4" };
        let title = url.split(['?', '#']).next().and_then(|p| p.rsplit('/').next()).filter(|t| !t.is_empty()).unwrap_or("Media").to_string();
        core.send_cmd(&id, DeviceCmd::Cast(vec![crate::types::CastItem { url, title, content_type: content_type.into(), subtitles: None, autoplay: true }]));
        return Ok(());
    }
    let ip = tv_ready(&core, &id).await?;
    let title = url.split(['?', '#']).next().and_then(|p| p.rsplit('/').next()).filter(|t| !t.is_empty()).unwrap_or("Video").to_string();
    let item = Item { fmt: stream_format_for_url(&url), url, title, subtitles: None, autoplay: true };
    crate::backends::roku_player::play(&ip, &[item]).await?;
    core.send_cmd(&id, DeviceCmd::Resync);
    Ok(())
}

#[tauri::command]
pub fn device_key(core: CoreState, id: String, key: String) {
    info!(id=%id, key=%key, "ui: remote key");
    core.send_cmd(&id, DeviceCmd::Key(key));
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
                e.info.sync_gain = imported.sync_gain;
            } else {
                let mut info = imported.clone();
                info.online = false;
                info.media = None;
                inner.devices.insert(info.id.clone(), crate::core::Entry { info, cmd: None, group_members: Vec::new() });
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
            macs: Vec::new(),
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

/// Opens THIRD-PARTY-NOTICES.txt, bundled as a resource.
#[tauri::command]
pub fn open_notices(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    use tauri_plugin_opener::OpenerExt;
    let path = app.path()
        .resolve("THIRD-PARTY-NOTICES.txt", tauri::path::BaseDirectory::Resource)
        .map_err(|e| e.to_string())?;
    app.opener().open_path(path.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_log_folder(app: tauri::AppHandle) {
    use tauri_plugin_opener::OpenerExt;
    let dir = crate::config::config_dir().join("logs");
    let _ = app.opener().open_path(dir.to_string_lossy(), None::<&str>);
}
