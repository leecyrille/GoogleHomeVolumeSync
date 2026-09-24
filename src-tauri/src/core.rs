//! Central state: device registry, actor management, volume-sync engine,
//! scheduler, persistence, frontend event emission.

use crate::backends::{lg::LgActor, optoma::OptomaActor, roku::RokuActor, yamaha::YamahaActor};
use crate::cast::connection::CastActor;
use crate::cast::discovery::DiscoveredCast;
use crate::config::{self, AppConfig, ManualDevice};
use crate::types::*;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tracing::{info, warn};

pub struct Entry {
    pub info: DeviceInfo,
    pub cmd: Option<mpsc::Sender<DeviceCmd>>,
    /// For cast groups: normalized member device ids from multizone status.
    pub group_members: Vec<String>,
}

pub struct CoreInner {
    pub cfg: AppConfig,
    pub devices: HashMap<String, Entry>,
    /// Echo suppression for the sync engine: id -> (expected volume, when set).
    pub pending: HashMap<String, (f32, Instant)>,
    pub cfg_dirty: bool,
    /// What the tray's now-playing section was last built from.
    pub tray_sig: String,
}

/// An active playback session, for the tray's now-playing section.
pub struct NowPlaying {
    pub id: String,
    pub device: String,
    pub playing: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub app: Option<String>,
}

pub struct Core {
    pub inner: Mutex<CoreInner>,
    pub event_tx: mpsc::Sender<CoreEvent>,
    pub lg_key_tx: mpsc::Sender<(String, String)>,
    pub app: AppHandle,
}

#[derive(Serialize, Clone)]
pub struct Snapshot {
    pub devices: Vec<DeviceInfo>,
    pub groups: Vec<AppGroup>,
    pub schedules: Vec<ScheduleEvent>,
    pub settings: crate::config::Settings,
}

pub fn now_ts() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

impl Core {
    pub fn new(app: AppHandle, event_tx: mpsc::Sender<CoreEvent>, lg_key_tx: mpsc::Sender<(String, String)>) -> Self {
        let cfg = config::load();
        let mut devices = HashMap::new();
        for mut info in cfg.known_devices.clone() {
            info.online = false;
            info.media = None;
            devices.insert(info.id.clone(), Entry { info, cmd: None, group_members: Vec::new() });
        }
        info!(count = devices.len(), "core: loaded known devices from config");
        Core {
            inner: Mutex::new(CoreInner { cfg, devices, pending: HashMap::new(), cfg_dirty: false, tray_sig: String::new() }),
            event_tx,
            lg_key_tx,
            app,
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.inner.lock().unwrap();
        // Overlay: members of a cast group with an active session show that
        // session as "via <group>" when they have no session of their own.
        let mut overlays: HashMap<String, crate::types::MediaInfo> = HashMap::new();
        for e in inner.devices.values() {
            if e.info.is_cast_group {
                if let Some(media) = &e.info.media {
                    for m in &e.group_members {
                        let mut via = media.clone();
                        via.app = Some(format!("via {}", e.info.custom_name.as_deref().unwrap_or(&e.info.friendly_name)));
                        via.supports_transport = false;
                        overlays.insert(m.clone(), via);
                    }
                }
            }
        }
        let by_norm: HashMap<String, String> = inner.devices.keys()
            .map(|id| (id.to_lowercase().replace('-', ""), id.clone()))
            .collect();
        let mut devices: Vec<DeviceInfo> = inner.devices.values().map(|e| {
            let mut info = e.info.clone();
            info.members = e.group_members.iter().filter_map(|m| by_norm.get(m).cloned()).collect();
            if info.media.is_none() {
                if let Some(via) = overlays.get(&info.id.to_lowercase().replace('-', "")) {
                    info.media = Some(via.clone());
                }
            }
            info
        }).collect();
        devices.sort_by(|a, b| {
            (b.online as u8, a.is_cast_group as u8)
                .cmp(&(a.online as u8, b.is_cast_group as u8))
                .then_with(|| a.friendly_name.to_lowercase().cmp(&b.friendly_name.to_lowercase()))
        });
        Snapshot {
            devices,
            groups: inner.cfg.groups.clone(),
            schedules: inner.cfg.schedules.clone(),
            settings: inner.cfg.settings.clone(),
        }
    }

    pub fn emit_state(&self) {
        let _ = self.app.emit("state", self.snapshot());
    }

    pub fn save_config(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.cfg.known_devices = inner.devices.values().map(|e| e.info.clone()).collect();
        config::save(&inner.cfg);
        inner.cfg_dirty = false;
    }

    // ---- device lifecycle -------------------------------------------------

    pub fn on_cast_discovered(&self, d: DiscoveredCast) {
        let mut spawn_needed = false;
        {
            let mut inner = self.inner.lock().unwrap();
            let entry = inner.devices.entry(d.uuid.clone());
            let entry = entry.or_insert_with(|| {
                info!(uuid=%d.uuid, name=%d.friendly_name, model=%d.model, ip=%d.ip, port=d.port, is_group=d.is_group, "core: NEW cast device discovered");
                Entry {
                    info: DeviceInfo {
                        id: d.uuid.clone(),
                        backend: Backend::Cast,
                        friendly_name: d.friendly_name.clone(),
                        custom_name: None,
                        model: d.model.clone(),
                        ip: d.ip.clone(),
                        port: d.port,
                        is_cast_group: d.is_group,
                        online: false,
                        last_seen: now_ts(),
                        volume: 0.0,
                        muted: false,
                        can_absolute_volume: true,
                        sync_gain: 1.0,
                        restricted: false,
                        power: None,
                        input: None,
                        inputs: Vec::new(),
                        members: Vec::new(),
                        media: None,
                    },
                    cmd: None,
                    group_members: Vec::new(),
                }
            });
            let addr_changed = entry.info.ip != d.ip || entry.info.port != d.port;
            entry.info.friendly_name = d.friendly_name.clone();
            entry.info.model = d.model.clone();
            entry.info.ip = d.ip.clone();
            entry.info.port = d.port;
            entry.info.last_seen = now_ts();
            if entry.cmd.is_none() {
                spawn_needed = true;
            } else if addr_changed {
                info!(uuid=%d.uuid, name=%d.friendly_name, ip=%d.ip, "core: cast device address changed, respawning actor");
                if let Some(cmd) = entry.cmd.take() {
                    let _ = cmd.try_send(DeviceCmd::Shutdown);
                }
                spawn_needed = true;
            }
            inner.cfg_dirty = true;
        }
        if spawn_needed {
            self.spawn_cast_actor(&d.uuid);
        }
        self.emit_state();
    }

    fn spawn_cast_actor(&self, id: &str) {
        let (tx, rx) = mpsc::channel(32);
        let (name, ip, port, is_group) = {
            let mut inner = self.inner.lock().unwrap();
            let entry = inner.devices.get_mut(id).unwrap();
            entry.cmd = Some(tx);
            (entry.info.friendly_name.clone(), entry.info.ip.clone(), entry.info.port, entry.info.is_cast_group)
        };
        let actor = CastActor {
            id: id.to_string(),
            name,
            ip,
            port,
            is_group,
            cmd_rx: rx,
            events: self.event_tx.clone(),
        };
        tauri::async_runtime::spawn(actor.run());
    }

    pub fn spawn_manual_actors(&self) {
        let manuals = self.inner.lock().unwrap().cfg.manual_devices.clone();
        for m in manuals {
            self.ensure_manual_device(&m);
        }
    }

    pub fn ensure_manual_device(&self, m: &ManualDevice) {
        let id = format!("{:?}:{}", m.backend, m.ip).to_lowercase();
        let (tx, rx) = mpsc::channel(32);
        {
            let mut inner = self.inner.lock().unwrap();
            let can_abs = m.backend != Backend::Roku;
            let entry = inner.devices.entry(id.clone()).or_insert_with(|| Entry {
                info: DeviceInfo {
                    id: id.clone(),
                    backend: m.backend,
                    friendly_name: m.name.clone(),
                    custom_name: None,
                    model: format!("{:?}", m.backend),
                    ip: m.ip.clone(),
                    port: m.port,
                    is_cast_group: false,
                    online: false,
                    last_seen: now_ts(),
                    volume: 0.0,
                    muted: false,
                    can_absolute_volume: can_abs,
                    sync_gain: 1.0,
                    restricted: false,
                    power: None,
                    input: None,
                    inputs: Vec::new(),
                    members: Vec::new(),
                    media: None,
                },
                cmd: None,
                group_members: Vec::new(),
            });
            if let Some(old) = entry.cmd.take() {
                let _ = old.try_send(DeviceCmd::Shutdown);
            }
            entry.cmd = Some(tx);
            entry.info.ip = m.ip.clone();
            entry.info.port = m.port;
            inner.cfg_dirty = true;
        }
        info!(id=%id, backend=?m.backend, ip=%m.ip, "core: spawning manual device actor");
        match m.backend {
            Backend::Roku => {
                let cached = self.inner.lock().unwrap().cfg.roku_levels.get(&id).copied();
                let actor = RokuActor {
                    id: id.clone(), name: m.name.clone(), ip: m.ip.clone(),
                    cmd_rx: rx, events: self.event_tx.clone(), cached_level: cached, macs: m.macs.clone(),
                };
                tauri::async_runtime::spawn(actor.run());
            }
            Backend::Yamaha => {
                let actor = YamahaActor {
                    id: id.clone(), name: m.name.clone(), ip: m.ip.clone(),
                    cmd_rx: rx, events: self.event_tx.clone(),
                };
                tauri::async_runtime::spawn(actor.run());
            }
            Backend::Lg => {
                let key = self.inner.lock().unwrap().cfg.lg_keys.get(&id).cloned();
                let actor = LgActor {
                    id: id.clone(), name: m.name.clone(), ip: m.ip.clone(), client_key: key,
                    cmd_rx: rx, events: self.event_tx.clone(), key_tx: self.lg_key_tx.clone(),
                };
                tauri::async_runtime::spawn(actor.run());
            }
            Backend::Optoma => {
                let actor = OptomaActor {
                    id: id.clone(), name: m.name.clone(), ip: m.ip.clone(), port: m.port,
                    cmd_rx: rx, events: self.event_tx.clone(),
                };
                tauri::async_runtime::spawn(actor.run());
            }
            Backend::Cast => {}
        }
        self.emit_state();
    }

    pub fn respawn_known_cast_devices(&self) {
        let ids: Vec<String> = {
            let inner = self.inner.lock().unwrap();
            inner.devices.values()
                .filter(|e| e.info.backend == Backend::Cast && e.cmd.is_none() && !e.info.ip.is_empty())
                .map(|e| e.info.id.clone())
                .collect()
        };
        for id in ids {
            self.spawn_cast_actor(&id);
        }
    }

    pub fn delete_device(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.devices.remove(id) {
            info!(id=%id, name=%entry.info.friendly_name, "core: device deleted by user");
            if let Some(cmd) = entry.cmd {
                let _ = cmd.try_send(DeviceCmd::Shutdown);
            }
        }
        for g in inner.cfg.groups.iter_mut() {
            g.member_ids.retain(|m| m != id);
        }
        inner.cfg_dirty = true;
        drop(inner);
        self.save_config();
        self.emit_state();
    }

    // ---- commands ---------------------------------------------------------

    pub fn send_cmd(&self, id: &str, cmd: DeviceCmd) {
        let sender = {
            let inner = self.inner.lock().unwrap();
            inner.devices.get(id).and_then(|e| e.cmd.clone())
        };
        match sender {
            Some(tx) => {
                let _ = tx.try_send(cmd);
            }
            None => warn!(id=%id, ?cmd, "core: command for device with no actor"),
        }
    }

    pub fn set_device_volume(&self, id: &str, level: f32, mark_pending: bool) {
        if mark_pending {
            self.inner.lock().unwrap().pending.insert(id.to_string(), (level, Instant::now()));
        }
        self.send_cmd(id, DeviceCmd::SetVolume(level));
    }

    fn gain_of(inner: &CoreInner, id: &str) -> f32 {
        inner.devices.get(id).map(|e| e.info.sync_gain).unwrap_or(1.0)
    }

    pub fn set_group_volume(&self, group_id: &str, level: f32) {
        // `level` is the group's logical volume; each member gets level x gain.
        let targets: Vec<(String, f32)> = {
            let mut inner = self.inner.lock().unwrap();
            let Some(g) = inner.cfg.groups.iter_mut().find(|g| g.id == group_id) else { return };
            g.group_volume = level;
            let members = g.member_ids.clone();
            inner.cfg_dirty = true;
            members.iter()
                .map(|m| (m.clone(), (level * Self::gain_of(&inner, m)).clamp(0.0, 1.0)))
                .collect()
        };
        info!(group=%group_id, level, targets=?targets, "core: set group volume");
        for (m, v) in targets {
            self.set_device_volume(&m, v, true);
        }
        self.emit_state();
    }

    pub fn set_sync_gain(&self, id: &str, gain: f32) {
        let gain = gain.clamp(0.0, 2.0);
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(e) = inner.devices.get_mut(id) {
                info!(id=%id, old=e.info.sync_gain, new=gain, "core: sync gain changed");
                e.info.sync_gain = gain;
            }
            inner.cfg_dirty = true;
        }
        self.save_config();
        self.emit_state();
    }

    pub fn group_transport(&self, group_id: &str, cmd: DeviceCmd) {
        // Send to members that actually have a media session. Grouped playback
        // (e.g. Spotify to a Google cast group) hosts its session on the cast
        // group device, not the member speakers - so if no member has a
        // session, fall back to online cast-group devices that are playing.
        let targets = {
            let inner = self.inner.lock().unwrap();
            let Some(g) = inner.cfg.groups.iter().find(|g| g.id == group_id) else { return };
            let with_session: Vec<String> = g.member_ids.iter()
                .filter(|m| inner.devices.get(*m).map(|e| e.info.media.is_some()).unwrap_or(false))
                .cloned().collect();
            if !with_session.is_empty() {
                with_session
            } else {
                inner.devices.values()
                    .filter(|e| e.info.is_cast_group && e.info.online && e.info.media.is_some())
                    .map(|e| e.info.id.clone())
                    .collect()
            }
        };
        info!(group=%group_id, ?cmd, ?targets, "core: group transport command");
        for t in targets {
            self.send_cmd(&t, cmd.clone());
        }
    }

    /// UI-originated volume change: set the device and mirror to synced peers.
    /// `level` is the device's ACTUAL volume; peers get logical x their gain,
    /// where logical = level / this device's gain.
    pub fn set_device_volume_from_ui(&self, id: &str, level: f32) {
        let peers: Vec<(String, f32)> = {
            let mut inner = self.inner.lock().unwrap();
            let logical = (level / Self::gain_of(&inner, id).max(0.05)).clamp(0.0, 1.0);
            let mut peers: Vec<(String, f32)> = Vec::new();
            let groups: Vec<AppGroup> = inner.cfg.groups.iter()
                .filter(|g| g.member_ids.contains(&id.to_string()))
                .cloned().collect();
            for g in groups {
                if let Some(gm) = inner.cfg.groups.iter_mut().find(|x| x.id == g.id) {
                    gm.group_volume = logical;
                }
                for m in &g.member_ids {
                    if m.as_str() != id && !peers.iter().any(|(p, _)| p == m) {
                        let target = (logical * Self::gain_of(&inner, m)).clamp(0.0, 1.0);
                        peers.push((m.clone(), target));
                    }
                }
            }
            if !peers.is_empty() {
                info!(id=%id, level, logical, ?peers, "core: sync - mirroring UI volume change to group peers");
                inner.cfg_dirty = true;
            }
            peers
        };
        self.set_device_volume(id, level, true);
        for (p, v) in peers {
            self.set_device_volume(&p, v, true);
        }
        self.emit_state();
    }

    // ---- now playing (tray) -------------------------------------------------

    /// Sessions that are playing or paused, playing first. Cast groups hold
    /// their own session, so a group cast shows once rather than per speaker.
    pub fn now_playing(&self) -> Vec<NowPlaying> {
        let inner = self.inner.lock().unwrap();
        let mut list: Vec<NowPlaying> = inner.devices.values()
            .filter(|e| e.info.online)
            .filter_map(|e| {
                let m = e.info.media.as_ref()?;
                if !m.supports_transport {
                    return None;
                }
                let playing = matches!(m.state.as_str(), "PLAYING" | "BUFFERING");
                if !playing && m.state != "PAUSED" {
                    return None;
                }
                Some(NowPlaying {
                    id: e.info.id.clone(),
                    device: e.info.custom_name.clone().unwrap_or_else(|| e.info.friendly_name.clone()),
                    playing,
                    title: m.title.clone(),
                    artist: m.artist.clone(),
                    app: m.app.clone(),
                })
            })
            .collect();
        list.sort_by(|a, b| b.playing.cmp(&a.playing).then_with(|| a.device.to_lowercase().cmp(&b.device.to_lowercase())));
        list
    }

    /// Rebuild the tray menu only when the set of sessions, their play/pause
    /// state or their track changes (BUFFERING counts as playing, so the
    /// constant PLAYING/BUFFERING flicker during streams doesn't rebuild it).
    pub fn refresh_tray_if_playback_changed(&self) {
        let sig: String = self.now_playing().iter()
            .map(|n| format!("{}|{}|{:?}|{:?}", n.id, n.playing, n.title, n.artist))
            .collect::<Vec<_>>()
            .join(";");
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.tray_sig == sig {
                return;
            }
            inner.tray_sig = sig;
        }
        crate::tray::rebuild_tray_menu(&self.app, self);
    }

    /// Play/pause toggle, decided from the session's current state.
    pub fn media_toggle(&self, id: &str) {
        let playing = {
            let inner = self.inner.lock().unwrap();
            inner.devices.get(id)
                .and_then(|e| e.info.media.as_ref())
                .map(|m| matches!(m.state.as_str(), "PLAYING" | "BUFFERING"))
        };
        match playing {
            Some(true) => self.send_cmd(id, DeviceCmd::Pause),
            Some(false) => self.send_cmd(id, DeviceCmd::Play),
            None => warn!(id=%id, "core: play/pause toggle for a device with no session"),
        }
    }

    // ---- event handling (incl. sync engine) --------------------------------

    pub fn handle_event(&self, ev: CoreEvent) {
        match ev {
            CoreEvent::Online { id, online } => {
                let mut inner = self.inner.lock().unwrap();
                if let Some(e) = inner.devices.get_mut(&id) {
                    e.info.online = online;
                    if online {
                        e.info.last_seen = now_ts();
                    } else {
                        e.info.media = None;
                    }
                    inner.cfg_dirty = true;
                }
                drop(inner);
                self.emit_state();
                self.refresh_tray_if_playback_changed();
            }
            CoreEvent::DeviceStatus { id, restricted, power, input, inputs, macs } => {
                let mut inner = self.inner.lock().unwrap();
                if let Some(e) = inner.devices.get_mut(&id) {
                    e.info.restricted = restricted;
                    e.info.power = power;
                    e.info.input = input;
                    e.info.inputs = inputs;
                }
                let key = id.clone();
                if let Some(m) = inner.cfg.manual_devices.iter_mut()
                    .find(|m| format!("{:?}:{}", m.backend, m.ip).to_lowercase() == key)
                {
                    if !macs.is_empty() && m.macs != macs {
                        m.macs = macs;
                        inner.cfg_dirty = true;
                    }
                }
                drop(inner);
                self.emit_state();
            }
            CoreEvent::GroupMembers { id, members } => {
                let mut inner = self.inner.lock().unwrap();
                if let Some(e) = inner.devices.get_mut(&id) {
                    e.group_members = members;
                }
                drop(inner);
                self.emit_state();
            }
            CoreEvent::MediaChanged { id, media } => {
                let mut inner = self.inner.lock().unwrap();
                if let Some(e) = inner.devices.get_mut(&id) {
                    e.info.media = media;
                }
                drop(inner);
                self.emit_state();
                self.refresh_tray_if_playback_changed();
            }
            CoreEvent::VolumeChanged { id, volume, muted } => {
                let mut sync_targets: Vec<(String, f32)> = Vec::new();
                {
                    let mut inner = self.inner.lock().unwrap();
                    let old = inner.devices.get(&id).map(|e| (e.info.volume, e.info.muted));
                    let mut is_roku = false;
                    if let Some(e) = inner.devices.get_mut(&id) {
                        e.info.volume = volume;
                        e.info.muted = muted;
                        e.info.last_seen = now_ts();
                        is_roku = e.info.backend == Backend::Roku;
                    }
                    if is_roku {
                        let lvl = (volume * 100.0).round() as u8;
                        inner.cfg.roku_levels.insert(id.clone(), lvl);
                    }
                    let changed = old.map(|(v, m)| (v - volume).abs() > 0.005 || m != muted).unwrap_or(true);
                    if changed {
                        if let Some((ov, om)) = old {
                            info!(id=%id, old_volume=ov, new_volume=volume, old_muted=om, muted, "core: volume changed");
                        }
                        // Echo suppression: is this our own SET_VOLUME coming back?
                        let is_echo = inner.pending.get(&id)
                            .map(|(exp, at)| (exp - volume).abs() <= 0.03 && at.elapsed() < Duration::from_secs(5))
                            .unwrap_or(false);
                        if is_echo {
                            inner.pending.remove(&id);
                        } else if changed && old.is_some() {
                            // External change: propagate to sync groups.
                            // Logical volume = actual / source gain; each peer
                            // gets logical x its own gain.
                            let logical = (volume / Self::gain_of(&inner, &id).max(0.05)).clamp(0.0, 1.0);
                            let groups: Vec<AppGroup> = inner.cfg.groups.iter()
                                .filter(|g| g.member_ids.contains(&id))
                                .cloned().collect();
                            for g in groups {
                                info!(group=%g.name, source=%id, volume, logical, "core: sync - propagating external volume change");
                                if let Some(gm) = inner.cfg.groups.iter_mut().find(|x| x.id == g.id) {
                                    gm.group_volume = logical;
                                }
                                for m in &g.member_ids {
                                    if m != &id {
                                        let target = (logical * Self::gain_of(&inner, m)).clamp(0.0, 1.0);
                                        let cur = inner.devices.get(m).map(|e| e.info.volume).unwrap_or(-1.0);
                                        if (cur - target).abs() > 0.01 {
                                            sync_targets.push((m.clone(), target));
                                        }
                                    }
                                }
                            }
                        }
                        inner.cfg_dirty = true;
                    }
                }
                for (m, v) in sync_targets {
                    self.set_device_volume(&m, v, true);
                }
                self.emit_state();
            }
        }
    }

    // ---- scheduler ---------------------------------------------------------

    pub fn run_due_schedules(&self, last_fired: &mut HashMap<String, String>) {
        use chrono::{Datelike, Local, Timelike};
        let now = Local::now();
        let hhmm = format!("{:02}:{:02}", now.hour(), now.minute());
        let day_idx = now.weekday().num_days_from_monday() as usize; // 0=Mon
        let stamp = format!("{}-{}", now.date_naive(), hhmm);
        let due: Vec<ScheduleEvent> = {
            let inner = self.inner.lock().unwrap();
            inner.cfg.schedules.iter()
                .filter(|s| s.enabled && s.days[day_idx] && s.time == hhmm)
                .filter(|s| last_fired.get(&s.id) != Some(&stamp))
                .cloned().collect()
        };
        for s in due {
            info!(schedule=%s.id, time=%s.time, "scheduler: firing event");
            last_fired.insert(s.id.clone(), stamp.clone());
            for t in &s.targets {
                let level = t.volume_pct as f32 / 100.0;
                if t.is_group {
                    self.set_group_volume(&t.target_id, level);
                } else {
                    info!(device=%t.target_id, level, "scheduler: set device volume");
                    self.set_device_volume(&t.target_id, level, true);
                }
            }
        }
    }
}
