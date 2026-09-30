//! Separate voice and music volumes for Google speakers.
//!
//! With nothing playing, a speaker sits at its voice volume (what the Assistant
//! answers at, 40% unless changed). When music starts it goes to its music
//! volume, which remembers every change made while music plays. The tray,
//! schedules, sync groups and sliders set the music volume; on an idle speaker
//! they only update the remembered level. A change made while idle is taken as
//! the music volume if music starts soon after; otherwise the voice volume
//! comes back.

use crate::core::{Core, CoreInner};
use crate::types::Backend;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// Nothing playing for this long: back to the voice volume.
const IDLE_AFTER: Duration = Duration::from_secs(15);
/// After the app starts, speakers and especially speaker groups take a while to report
/// what they're playing; decide nothing until then, or music could drop to the voice volume.
const STARTUP_GRACE: Duration = Duration::from_secs(60);
/// Paused this long counts as stopped.
const PAUSE_ENDS_MUSIC: Duration = Duration::from_secs(5 * 60);
/// A change made while idle waits this long for music before the voice volume returns.
const INTENT_WINDOW: Duration = Duration::from_secs(45);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Voice,
    Music,
}

#[derive(Default)]
struct State {
    mode: HashMap<String, Mode>,
    idle_since: HashMap<String, Instant>,
    paused_since: HashMap<String, Instant>,
    /// A volume someone set while the speaker was idle, and when.
    intent: HashMap<String, (f32, Instant)>,
}

fn st() -> MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap()
}

/// Google speakers and displays with the Assistant (not Chromecasts, cast groups or TVs).
pub fn eligible(inner: &CoreInner, id: &str) -> bool {
    inner.devices.get(id).map(|e| {
        let m = e.info.model.to_ascii_lowercase();
        e.info.backend == Backend::Cast && !e.info.is_cast_group && !m.contains("chromecast")
            && (m.contains("google home") || m.contains("nest") || m.contains("home max") || m.contains("home mini"))
    }).unwrap_or(false)
}

pub fn enabled(inner: &CoreInner) -> bool {
    inner.cfg.voice_volume.enabled
}

pub fn voice_level(inner: &CoreInner, id: &str) -> f32 {
    let c = &inner.cfg.voice_volume;
    c.per_device.get(id).copied().unwrap_or(c.voice).clamp(0.0, 1.0)
}

pub fn mode(id: &str) -> Option<Mode> {
    st().mode.get(id).copied()
}

/// True when a speaker managed here is idle, so a music-volume change should only be remembered.
pub fn voice_mode_now(inner: &CoreInner, id: &str) -> bool {
    enabled(inner) && eligible(inner, id) && mode(id) == Some(Mode::Voice)
}

/// Someone changed the volume on the speaker itself (or the Google Home app) while idle.
pub fn note_idle_change(id: &str, volume: f32) {
    st().intent.insert(id.to_string(), (volume, Instant::now()));
}

/// What counts as music: something playing (or recently paused) on the speaker or a group around it.
/// The calendar picture and broadcast messages don't count.
fn playing_state(inner: &CoreInner, id: &str) -> Option<String> {
    let is_ours = |m: &crate::types::MediaInfo| m.app.as_deref() == Some("Default Media Receiver")
        && matches!(m.title.as_deref(), Some("Calendar" | "Message"));
    let own = inner.devices.get(id).and_then(|e| e.info.media.as_ref()).filter(|m| !is_ours(m)).map(|m| m.state.clone());
    let norm = id.to_lowercase().replace('-', "");
    let via_group = inner.devices.values()
        .filter(|g| g.info.is_cast_group && g.group_members.contains(&norm))
        .filter_map(|g| g.info.media.as_ref().filter(|m| !is_ours(m)).map(|m| m.state.clone()))
        .max_by_key(|s| matches!(s.as_str(), "PLAYING" | "BUFFERING") as u8);
    match (own, via_group) {
        (Some(a), _) if matches!(a.as_str(), "PLAYING" | "BUFFERING") => Some(a),
        (_, Some(b)) if matches!(b.as_str(), "PLAYING" | "BUFFERING") => Some(b),
        (a, b) => a.or(b),
    }
}

pub async fn run(core: Arc<Core>) {
    tokio::time::sleep(STARTUP_GRACE).await;
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        step(&core);
    }
}

fn step(core: &Core) {
    let mut sets: Vec<(String, f32)> = Vec::new();
    let mut changed = false;
    {
        let mut inner = core.inner.lock().unwrap();
        if !enabled(&inner) {
            st().mode.clear();
            return;
        }
        let ids: Vec<String> = inner.devices.iter()
            .filter(|(id, e)| e.info.online && eligible(&inner, id))
            .map(|(id, _)| id.clone()).collect();
        let mut s = st();
        for id in ids {
            if inner.hold_sync.contains(&id) {
                continue; // a broadcast is using this speaker
            }
            let state = playing_state(&inner, &id);
            let now = Instant::now();
            let music = match state.as_deref() {
                Some("PLAYING" | "BUFFERING") => {
                    s.paused_since.remove(&id);
                    true
                }
                Some("PAUSED") => s.paused_since.entry(id.clone()).or_insert(now).elapsed() < PAUSE_ENDS_MUSIC,
                _ => {
                    s.paused_since.remove(&id);
                    false
                }
            };
            let actual = inner.devices.get(&id).map(|e| e.info.volume).unwrap_or(0.0);
            let mode = s.mode.get(&id).copied();
            if music {
                s.idle_since.remove(&id);
                if mode != Some(Mode::Music) {
                    // A level set just before the music started is what they wanted.
                    let target = match s.intent.remove(&id).filter(|(_, at)| at.elapsed() < INTENT_WINDOW) {
                        Some((v, _)) => v,
                        None => inner.cfg.voice_volume.music.get(&id).copied().unwrap_or(actual),
                    };
                    inner.cfg.voice_volume.music.insert(id.clone(), target);
                    inner.cfg_dirty = true;
                    s.mode.insert(id.clone(), Mode::Music);
                    info!(id=%id, target, "voice/music: music playing; music volume");
                    if (actual - target).abs() > 0.01 {
                        sets.push((id.clone(), target));
                    }
                    changed = true;
                }
            } else {
                let since = *s.idle_since.entry(id.clone()).or_insert(now);
                let voice = voice_level(&inner, &id);
                let intent_fresh = s.intent.get(&id).map(|(_, at)| at.elapsed() < INTENT_WINDOW).unwrap_or(false);
                if mode != Some(Mode::Voice) {
                    if since.elapsed() >= IDLE_AFTER {
                        s.mode.insert(id.clone(), Mode::Voice);
                        let norm = id.to_lowercase().replace('-', "");
                        let groups: Vec<String> = inner.devices.values().filter(|g| g.info.is_cast_group)
                            .map(|g| format!("{}:{}:{:?}:{}", g.info.friendly_name, g.group_members.contains(&norm), g.info.media.as_ref().map(|m| m.state.clone()), g.group_members.len()))
                            .collect();
                        debug!(?groups, "voice/music: groups around the speaker");
                        info!(id=%id, voice, "voice/music: nothing playing; voice volume");
                        if (actual - voice).abs() > 0.01 {
                            sets.push((id.clone(), voice));
                        }
                        changed = true;
                    }
                } else if !intent_fresh && (actual - voice).abs() > 0.02 && !inner.pending.contains_key(&id) {
                    let norm = id.to_lowercase().replace('-', "");
                    let groups: Vec<String> = inner.devices.values().filter(|g| g.info.is_cast_group)
                        .map(|g| format!("{}:{}:{:?}:{}", g.info.friendly_name, g.group_members.contains(&norm), g.info.media.as_ref().map(|m| m.state.clone()), g.group_members.len()))
                        .collect();
                    debug!(id=%id, ?groups, own=?inner.devices.get(&id).and_then(|e| e.info.media.as_ref()).map(|m| m.state.clone()), "voice/music: groups around the speaker");
                    // Changed while idle and no music followed: the voice volume comes back.
                    s.intent.remove(&id);
                    info!(id=%id, voice, actual, "voice/music: back to the voice volume");
                    sets.push((id.clone(), voice));
                }
            }
        }
    }
    for (id, v) in sets {
        core.set_device_volume(&id, v, true);
    }
    if changed {
        core.emit_state();
    }
}
