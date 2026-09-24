//! Keeps one file playing in step across several devices.
//!
//! Each device loads the file paused; once all are ready they're started
//! together. After that the positions are sampled about once a second:
//! - a pause, resume or seek made on any one device is copied to the others;
//! - a device ahead of the slowest one by more than SYNC_TOLERANCE is paused
//!   for exactly its lead, then resumed, so it lines up;
//! - a gap too large for that (buffering, a missed seek) is fixed with a seek.

use crate::core::Core;
use crate::types::{DeviceCmd, MediaInfo};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Leads smaller than this are left alone.
const SYNC_TOLERANCE_MS: u64 = 200;
/// Beyond this, pausing would take too long: seek instead.
const SEEK_GAP_MS: u64 = 4000;
/// A position jump bigger than this, not explained by elapsed time, is a user seek.
const JUMP_MS: i64 = 2500;
/// Wait this long after our own commands before judging state again.
const SETTLE: Duration = Duration::from_millis(2500);

#[derive(Clone, Debug)]
pub struct SyncSession {
    pub id: u64,
    pub members: Vec<String>,
    pub paused: bool,
    /// Latest spread between the furthest-ahead and furthest-behind device.
    pub spread_ms: Option<u64>,
    pub status: String,
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Position now, advanced from when it was reported while playing.
fn estimate(m: &MediaInfo) -> Option<i64> {
    let pos = m.position_ms? as i64;
    if m.state == "PLAYING" {
        Some(pos + (unix_ms() - m.position_at.unwrap_or_else(unix_ms)).max(0))
    } else {
        Some(pos)
    }
}

fn still_current(core: &Core, id: u64) -> bool {
    core.inner.lock().unwrap().sync.as_ref().map(|s| s.id == id).unwrap_or(false)
}

fn set_status(core: &Core, id: u64, f: impl FnOnce(&mut SyncSession)) {
    {
        let mut inner = core.inner.lock().unwrap();
        match inner.sync.as_mut() {
            Some(s) if s.id == id => f(s),
            _ => return,
        }
    }
    core.emit_state();
}

fn media_of(core: &Core, members: &[String]) -> HashMap<String, Option<MediaInfo>> {
    let inner = core.inner.lock().unwrap();
    members.iter().map(|m| (m.clone(), inner.devices.get(m).and_then(|e| e.info.media.clone()))).collect()
}

fn poll_all(core: &Core, members: &[String]) {
    for m in members {
        core.send_cmd(m, DeviceCmd::PollMedia);
    }
}

pub async fn run(core: Arc<Core>, id: u64) {
    let members = match core.inner.lock().unwrap().sync.as_ref() {
        Some(s) if s.id == id => s.members.clone(),
        _ => return,
    };
    info!(id, ?members, "sync play: waiting for devices to load");
    set_status(&core, id, |s| s.status = "Loading on every device…".into());

    // 1. Wait (up to 20 s) until every device has the file loaded and paused.
    let started = Instant::now();
    let deadline = started + Duration::from_secs(20);
    loop {
        if !still_current(&core, id) {
            return;
        }
        poll_all(&core, &members);
        tokio::time::sleep(Duration::from_millis(700)).await;
        let media = media_of(&core, &members);
        // Some players report "buffering" while waiting to start; accept that after a few seconds.
        let patient = started.elapsed() > Duration::from_secs(6);
        let ready = media.values().all(|m| m.as_ref().map(|m| {
            m.state == "PAUSED" || (patient && m.state == "BUFFERING")
        }).unwrap_or(false));
        if ready {
            break;
        }
        if Instant::now() > deadline {
            warn!(id, "sync play: not every device reported ready; starting anyway");
            break;
        }
    }

    // 2. Start together.
    for m in &members {
        core.send_cmd(m, DeviceCmd::Play);
    }
    info!(id, "sync play: started");
    set_status(&core, id, |s| s.status = "Playing in sync".into());
    let mut quiet_until = Instant::now() + SETTLE;
    let mut last_seen: HashMap<String, (i64, Instant)> = HashMap::new();
    let mut gone_since: HashMap<String, Instant> = HashMap::new();

    // 3. Keep them together.
    loop {
        tokio::time::sleep(Duration::from_millis(650)).await;
        if !still_current(&core, id) {
            return;
        }
        let members = match core.inner.lock().unwrap().sync.as_ref() {
            Some(s) => s.members.clone(),
            None => return,
        };
        poll_all(&core, &members);
        tokio::time::sleep(Duration::from_millis(350)).await;
        let media = media_of(&core, &members);
        let now = Instant::now();

        // Devices that stopped playing this file (closed, switched apps) leave the session.
        for (m, info) in &media {
            let active = info.as_ref().map(|i| i.state != "IDLE").unwrap_or(false);
            if active {
                gone_since.remove(m);
            } else {
                gone_since.entry(m.clone()).or_insert(now);
            }
        }
        let leaving: Vec<String> = gone_since.iter()
            .filter(|(_, t)| now.duration_since(**t) > Duration::from_secs(10))
            .map(|(m, _)| m.clone()).collect();
        if !leaving.is_empty() {
            info!(id, ?leaving, "sync play: devices left");
            set_status(&core, id, |s| s.members.retain(|m| !leaving.contains(m)));
            let remaining = core.inner.lock().unwrap().sync.as_ref().map(|s| s.members.len()).unwrap_or(0);
            if remaining < 2 {
                info!(id, "sync play: fewer than two devices left; ending");
                let mut inner = core.inner.lock().unwrap();
                if inner.sync.as_ref().map(|s| s.id == id).unwrap_or(false) {
                    inner.sync = None;
                }
                drop(inner);
                core.emit_state();
                return;
            }
            continue;
        }

        let paused_session = core.inner.lock().unwrap().sync.as_ref().map(|s| s.paused).unwrap_or(false);
        let states: HashMap<&String, &str> = media.iter()
            .filter_map(|(m, i)| i.as_ref().map(|i| (m, i.state.as_str()))).collect();
        let estimates: HashMap<&String, i64> = media.iter()
            .filter_map(|(m, i)| i.as_ref().and_then(estimate).map(|e| (m, e))).collect();

        if now >= quiet_until {
            // Someone paused one device: pause the rest.
            if !paused_session {
                if let Some((who, _)) = states.iter().find(|(_, st)| **st == "PAUSED") {
                    info!(id, device=%who, "sync play: paused on one device; pausing all");
                    for m in &members {
                        if m != *who {
                            core.send_cmd(m, DeviceCmd::Pause);
                        }
                    }
                    set_status(&core, id, |s| { s.paused = true; s.status = "Paused".into(); });
                    quiet_until = now + SETTLE;
                    continue;
                }
            } else if let Some((who, _)) = states.iter().find(|(_, st)| **st == "PLAYING") {
                // Someone resumed: resume the rest (drift is fixed on the next pass).
                info!(id, device=%who, "sync play: resumed on one device; resuming all");
                for m in &members {
                    if m != *who {
                        core.send_cmd(m, DeviceCmd::Play);
                    }
                }
                set_status(&core, id, |s| { s.paused = false; s.status = "Playing in sync".into(); });
                quiet_until = now + SETTLE;
                last_seen.clear();
                continue;
            }
        }
        if paused_session || estimates.len() < 2 {
            continue;
        }

        let min = *estimates.values().min().unwrap();
        let max = *estimates.values().max().unwrap();
        let spread = (max - min) as u64;
        set_status(&core, id, |s| s.spread_ms = Some(spread));

        if now < quiet_until {
            for (m, e) in &estimates {
                last_seen.insert((*m).clone(), (*e, now));
            }
            continue;
        }

        if spread > SEEK_GAP_MS {
            // A big gap: follow whoever jumped (a user seek), else meet in the middle.
            let jumped = estimates.iter().filter_map(|(m, e)| {
                let (prev, at) = last_seen.get(*m)?;
                let expected = prev + now.duration_since(*at).as_millis() as i64;
                let off = (e - expected).abs();
                (off > JUMP_MS).then_some((*m, *e, off))
            }).max_by_key(|(_, _, off)| *off);
            let target = match jumped {
                Some((who, e, _)) => {
                    info!(id, device=%who, position=e, "sync play: seek on one device; moving the others");
                    e
                }
                None => {
                    let mut v: Vec<i64> = estimates.values().copied().collect();
                    v.sort();
                    v[v.len() / 2]
                }
            };
            for (m, e) in &estimates {
                if (e - target).abs() > SYNC_TOLERANCE_MS as i64 {
                    // Aim slightly ahead to cover the time the seek takes.
                    core.send_cmd(m, DeviceCmd::Seek((target + 400).max(0) as u64));
                }
            }
            quiet_until = now + SETTLE + Duration::from_secs(1);
            last_seen.clear();
            continue;
        }

        if spread > SYNC_TOLERANCE_MS {
            // Pause each device that's ahead for exactly its lead, then resume it.
            let mut longest = 0u64;
            for (m, e) in &estimates {
                let lead = (e - min) as u64;
                if lead > SYNC_TOLERANCE_MS / 2 {
                    longest = longest.max(lead);
                    let core = core.clone();
                    let m = (*m).clone();
                    core.send_cmd(&m, DeviceCmd::Pause);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(lead)).await;
                        core.send_cmd(&m, DeviceCmd::Play);
                    });
                }
            }
            info!(id, spread, "sync play: nudged devices that were ahead");
            quiet_until = now + Duration::from_millis(longest) + SETTLE;
            last_seen.clear();
            continue;
        }

        for (m, e) in &estimates {
            last_seen.insert((*m).clone(), (*e, now));
        }
    }
}
