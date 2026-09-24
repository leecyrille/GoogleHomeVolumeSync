//! Shows the PactoTech Calendar Saver on TVs.
//!
//! The saver is a separate Windows screensaver
//! (github.com/leecyrille/Windows-11-ICS-Calendar-Screensaver). Its render mode
//! draws the calendar in an off-screen window and saves it as a picture every
//! minute. While any TV wants the calendar we keep that running, serve the
//! picture at a lasting address, send each new one to Google screens, and let
//! Roku TVs (our player channel, which is also a Roku screensaver) fetch it
//! themselves.

use crate::core::Core;
use crate::types::{Backend, CastItem, DeviceCmd};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// Seconds between pictures.
pub const EVERY_SECS: u64 = 60;
const SIZE: &str = "1920x1080";
const PICTURE_NAME: &str = "calendar.jpg";
/// Screens get this long to switch to the calendar before we check they still show it.
const SETTLE: Duration = Duration::from_secs(30);
/// Checks in a row a screen must show something else before it stops getting the calendar.
const ELSEWHERE_LIMIT: u8 = 4;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub const NO_SAVER: &str = "Couldn't find the PactoTech Calendar Saver on this PC. Install it, or use Locate to pick the .scr file.";
const OLD_SAVER: &str = "This copy of the Calendar Saver can't draw for TVs yet. Update it to the latest version.";

#[derive(Default)]
struct Runtime {
    child: Option<std::process::Child>,
    started: Option<Instant>,
    started_at: Option<SystemTime>,
    retry_at: Option<Instant>,
    saver: Option<PathBuf>,
    saver_checked: Option<Instant>,
    /// Screens showing the calendar now, and since when.
    showing: HashMap<String, Instant>,
    /// Google screens still waiting for their first picture.
    needs_push: HashSet<String>,
    elsewhere: HashMap<String, u8>,
    pushed: Option<SystemTime>,
    error: Option<String>,
    outdated: Vec<String>,
}

fn rt() -> MutexGuard<'static, Runtime> {
    static RT: OnceLock<Mutex<Runtime>> = OnceLock::new();
    RT.get_or_init(Default::default).lock().unwrap()
}

/// For the UI.
#[derive(Serialize, Clone, Default)]
pub struct CalendarView {
    pub saver: Option<String>,
    pub rendering: bool,
    /// When the latest picture was made (unix ms).
    pub updated: Option<i64>,
    pub error: Option<String>,
    pub showing: Vec<String>,
    pub screensaver_tvs: Vec<String>,
    /// Screensaver TVs holding an old address for the picture (this PC's address changed).
    pub outdated: Vec<String>,
}

pub fn image_path() -> PathBuf {
    crate::config::config_dir().join(PICTURE_NAME)
}

fn modified(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

pub fn view(cfg: &crate::config::CalendarConfig) -> CalendarView {
    let r = rt();
    CalendarView {
        saver: r.saver.as_ref().map(|p| p.display().to_string()),
        rendering: r.child.is_some(),
        updated: modified(&image_path()).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64),
        error: r.error.clone(),
        showing: r.showing.keys().cloned().collect(),
        screensaver_tvs: cfg.screensaver_tvs.clone(),
        outdated: r.outdated.clone(),
    }
}

// ---------------- finding the saver ----------------

fn is_calendar_saver(p: &Path) -> bool {
    p.file_name().and_then(|n| n.to_str()).map(|n| n.to_ascii_lowercase().contains("calendarsaver")).unwrap_or(false)
}

/// The screensaver Windows is set to use (long file name).
fn current_screensaver() -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("reg")
        .args(["query", r"HKCU\Control Panel\Desktop", "/v", "SCRNSAVE.EXE"])
        .creation_flags(CREATE_NO_WINDOW)
        .output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let value = text.lines().find_map(|l| l.split_once("REG_SZ").map(|(_, v)| v.trim().to_string()))?;
    let long = std::fs::canonicalize(&value).ok()?;
    let s = long.to_string_lossy().to_string();
    Some(PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s)))
}

/// The chosen file, else the Windows screensaver if it's the Calendar Saver, else the usual install places.
fn find_saver(chosen: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = chosen.map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    if let Some(p) = current_screensaver().filter(|p| is_calendar_saver(p) && p.is_file()) {
        return Some(p);
    }
    let windows = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    ["System32", "SysWOW64"].iter()
        .map(|d| PathBuf::from(&windows).join(d).join("PactoTechCalendarSaver.scr"))
        .find(|p| p.is_file())
}

/// Where the saver is (looked up at most once a minute unless `force`).
pub fn saver(chosen: Option<String>, force: bool) -> Option<PathBuf> {
    let stale = {
        let r = rt();
        force || r.saver_checked.map(|t| t.elapsed() > Duration::from_secs(60)).unwrap_or(true)
    };
    if stale {
        let found = find_saver(chosen.as_deref());
        let mut r = rt();
        r.saver = found;
        r.saver_checked = Some(Instant::now());
    }
    rt().saver.clone()
}

// ---------------- what the commands change ----------------

/// The secret in the picture's address, made once and kept so screensavers keep working.
pub fn ensure_token(core: &Core) -> String {
    let mut inner = core.inner.lock().unwrap();
    if inner.cfg.calendar.token.is_empty() {
        inner.cfg.calendar.token = uuid::Uuid::new_v4().simple().to_string();
        inner.cfg_dirty = true;
    }
    inner.cfg.calendar.token.clone()
}

pub fn start_showing(id: &str) {
    let mut r = rt();
    r.showing.insert(id.to_string(), Instant::now());
    r.needs_push.insert(id.to_string());
    r.elsewhere.remove(id);
    r.retry_at = None;
}

/// True if it was showing.
pub fn stop_showing(id: &str) -> bool {
    let mut r = rt();
    r.needs_push.remove(id);
    r.elsewhere.remove(id);
    r.showing.remove(id).is_some()
}

/// Look for the saver again and retry it straight away (after the user picks or updates it).
pub fn reset() {
    let mut r = rt();
    r.saver_checked = None;
    r.retry_at = None;
    r.error = None;
}

/// The picture's address for a TV at `ip`.
pub async fn picture_url(token: &str, ip: &str) -> Result<String, String> {
    crate::media_server::live_url(token, PICTURE_NAME, ip).await
}

// ---------------- keeping it going ----------------

struct Dev {
    backend: Backend,
    ip: String,
    media_title: Option<String>,
    tv_showing: Option<String>,
}

fn start_renderer(saver: &Path) -> Result<std::process::Child, String> {
    use std::os::windows::process::CommandExt;
    // "/p render" rather than a new switch: older savers quietly exit on /p
    // (the Windows preview flag) instead of opening their settings window.
    std::process::Command::new(saver)
        .args(["/p", "render"])
        .arg(image_path())
        .args(["--size", SIZE, "--every", &EVERY_SECS.to_string(), "--parent", &std::process::id().to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("Couldn't start the Calendar Saver: {e}"))
}

pub async fn run(core: Arc<Core>) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tick.tick().await;
        step(&core).await;
    }
}

/// One pass: drop screens that moved on, run the renderer while needed, decide
/// who may fetch the picture, and send new pictures to Google screens.
pub async fn step(core: &Arc<Core>) {
    let (cfg, devices) = {
        let inner = core.inner.lock().unwrap();
        let devices: HashMap<String, Dev> = inner.devices.iter().map(|(id, e)| (id.clone(), Dev {
            backend: e.info.backend,
            ip: e.info.ip.clone(),
            media_title: e.info.media.as_ref().and_then(|m| m.title.clone()),
            tv_showing: e.info.tv.as_ref().and_then(|t| t.showing.clone()),
        })).collect();
        (inner.cfg.calendar.clone(), devices)
    };
    let mut changed = false;

    // 1. Screens that moved on to something else stop getting the calendar.
    {
        let mut r = rt();
        let ids: Vec<(String, Instant)> = r.showing.iter().map(|(k, v)| (k.clone(), *v)).collect();
        for (id, since) in ids {
            if since.elapsed() < SETTLE {
                continue;
            }
            let elsewhere = match devices.get(&id) {
                None => true,
                Some(d) => match d.backend {
                    Backend::Cast => d.media_title.as_deref().map(|t| t != "Calendar").unwrap_or(false),
                    Backend::Roku => d.tv_showing.as_deref().map(|s| !s.contains("Volume Sync")).unwrap_or(false),
                    _ => true,
                },
            };
            let n = r.elsewhere.entry(id.clone()).or_insert(0);
            *n = if elsewhere { *n + 1 } else { 0 };
            if *n >= ELSEWHERE_LIMIT {
                info!(id=%id, "calendar: screen moved on to something else");
                r.showing.remove(&id);
                r.elsewhere.remove(&id);
                r.needs_push.remove(&id);
                changed = true;
            }
        }
    }

    // 2. Run the saver's render mode while anything needs it.
    let need = !rt().showing.is_empty() || !cfg.screensaver_tvs.is_empty();
    let saver_path = if need { saver(cfg.saver_path.clone(), false) } else { None };
    {
        let mut r = rt();
        let made_since_start = |r: &Runtime| modified(&image_path()).zip(r.started_at).map(|(m, s)| m >= s).unwrap_or(false);
        let exited = r.child.as_mut().and_then(|c| c.try_wait().ok().flatten());
        if let Some(status) = exited {
            let quick = r.started.map(|t| t.elapsed() < Duration::from_secs(15)).unwrap_or(false);
            let old = quick && !made_since_start(&r);
            warn!(%status, old, "calendar: saver stopped");
            r.error = Some(if old { OLD_SAVER.to_string() } else { format!("The Calendar Saver stopped ({status}).") });
            r.child = None;
            r.retry_at = Some(Instant::now() + if old { Duration::from_secs(600) } else { Duration::from_secs(30) });
            changed = true;
        }
        if need && r.child.is_none() && r.retry_at.map(|t| Instant::now() >= t).unwrap_or(true) {
            match &saver_path {
                Some(path) => match start_renderer(path) {
                    Ok(child) => {
                        info!(saver=%path.display(), "calendar: drawing the calendar for TVs");
                        r.child = Some(child);
                        r.started = Some(Instant::now());
                        r.started_at = Some(SystemTime::now());
                        r.error = None;
                        changed = true;
                    }
                    Err(e) => {
                        r.error = Some(e);
                        r.retry_at = Some(Instant::now() + Duration::from_secs(60));
                        changed = true;
                    }
                },
                None => {
                    if r.error.as_deref() != Some(NO_SAVER) {
                        r.error = Some(NO_SAVER.into());
                        changed = true;
                    }
                }
            }
        }
        // Running but silent for a while.
        let silent = r.child.is_some()
            && r.started.map(|t| t.elapsed() > Duration::from_secs(150)).unwrap_or(false)
            && modified(&image_path()).map(|m| m.elapsed().map(|e| e > Duration::from_secs(EVERY_SECS * 3)).unwrap_or(false)).unwrap_or(true);
        if silent && r.error.is_none() {
            r.error = Some("The Calendar Saver is running but hasn't made a new picture for a few minutes.".into());
            changed = true;
        } else if !silent && r.child.is_some() && r.error.as_deref().map(|e| e.contains("hasn't made")).unwrap_or(false) {
            r.error = None;
            changed = true;
        }
        if !need {
            if let Some(mut child) = r.child.take() {
                let _ = child.kill();
                let _ = child.wait();
                info!("calendar: no TV needs the calendar; stopped drawing it");
                changed = true;
            }
            if r.error.is_some() {
                r.error = None;
                changed = true;
            }
        }
    }

    // 3. Only these TVs may fetch the picture.
    let token = cfg.token.clone();
    let wanted: Vec<String> = {
        let r = rt();
        r.showing.keys().cloned().chain(cfg.screensaver_tvs.iter().cloned()).collect::<HashSet<_>>().into_iter().collect()
    };
    let allowed: Vec<std::net::IpAddr> = wanted.iter().filter_map(|id| devices.get(id)?.ip.parse().ok()).collect();
    if !token.is_empty() {
        crate::media_server::set_live(&token, &image_path(), allowed).await;
    }

    // 4. New pictures go to the Google screens (Roku TVs fetch their own).
    let latest = modified(&image_path());
    let started_at = rt().started_at;
    let fresh = latest.zip(started_at).map(|(m, s)| m >= s).unwrap_or(false);
    if fresh && !token.is_empty() {
        let targets: Vec<String> = {
            let r = rt();
            let is_new = latest != r.pushed;
            r.showing.keys()
                .filter(|id| devices.get(*id).map(|d| d.backend == Backend::Cast).unwrap_or(false))
                .filter(|id| is_new || r.needs_push.contains(*id))
                .cloned().collect()
        };
        let version = latest.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        for id in &targets {
            let Some(d) = devices.get(id) else { continue };
            match picture_url(&token, &d.ip).await {
                Ok(url) => core.send_cmd(id, DeviceCmd::Cast(vec![CastItem {
                    url: format!("{url}?v={version}"),
                    title: "Calendar".into(),
                    content_type: "image/jpeg".into(),
                    subtitles: None,
                    autoplay: true,
                }])),
                Err(e) => warn!(id=%id, error=%e, "calendar: no address for the picture"),
            }
        }
        let mut r = rt();
        r.pushed = latest;
        for id in &targets {
            r.needs_push.remove(id);
        }
    }

    // 5. Screensaver TVs whose saved address went stale (this PC's address or port changed).
    if !token.is_empty() {
        let mut outdated = Vec::new();
        for id in &cfg.screensaver_tvs {
            let Some(d) = devices.get(id) else { continue };
            if let Ok(url) = picture_url(&token, &d.ip).await {
                if cfg.pushed.get(id) != Some(&url) {
                    outdated.push(id.clone());
                }
            }
        }
        let mut r = rt();
        if r.outdated != outdated {
            r.outdated = outdated;
            changed = true;
        }
    }

    if changed {
        core.emit_state();
    }
}
