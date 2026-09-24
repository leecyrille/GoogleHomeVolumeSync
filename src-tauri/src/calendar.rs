//! The calendar on TVs.
//!
//! cal_render draws the pictures; this module decides who shows them: TVs the
//! user picked, Roku TVs using it as their screensaver, and scheduled times
//! (which can turn a TV on, avoid interrupting a show, and turn it off again).
//! Pictures are served at a lasting address; Google screens are sent each new
//! one, while Roku TVs (our player channel) fetch their own and switch views
//! with Up / Down on the remote.

use crate::cal_render::{file_name, Variant, VIEWS};
use crate::config::{CalSchedule, CalendarConfig, DisplaySettings};
use crate::core::Core;
use crate::types::{Backend, CastItem, DeviceCmd};
use chrono::{Datelike, Local, TimeZone};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use tracing::{info, warn};

/// Screens get this long to switch to the calendar before we check they still show it.
const SETTLE: Duration = Duration::from_secs(30);
/// Checks in a row a screen must show something else before it stops getting the calendar.
const ELSEWHERE_LIMIT: u8 = 4;

/// How a screen shows the calendar.
#[derive(Clone, Debug, PartialEq)]
pub struct ShowOpts {
    pub variant: Variant,
    pub views: Vec<String>,
    /// Seconds between views (0 = stay on the first).
    pub rotate: u32,
}

struct Showing {
    ip: String,
    since: Instant,
    touched: Instant,
    opts: ShowOpts,
    /// The scheduled time that put it there, if any.
    window: Option<String>,
    /// Last picture sent to a Google screen: file and its time.
    pushed: Option<(String, SystemTime)>,
}

struct Win {
    end: chrono::DateTime<Local>,
    shown: bool,
    turned_on: bool,
    power_asked: Option<Instant>,
    done: bool,
}

#[derive(Default)]
struct Runtime {
    showing: HashMap<String, Showing>,
    elsewhere: HashMap<String, u8>,
    outdated: Vec<String>,
    windows: HashMap<String, Win>,
    last_schedules: Option<Instant>,
}

fn rt() -> MutexGuard<'static, Runtime> {
    static RT: OnceLock<Mutex<Runtime>> = OnceLock::new();
    RT.get_or_init(Default::default).lock().unwrap()
}

/// For the UI.
#[derive(Serialize, Clone, Default)]
pub struct CalendarView {
    #[serde(flatten)]
    pub render: crate::cal_render::RenderStatus,
    pub showing: Vec<String>,
    pub screensaver_tvs: Vec<String>,
    pub outdated: Vec<String>,
    pub settings: CalendarConfig,
    /// The PactoTech Calendar Saver's settings exist on this PC (offer to import them).
    pub saver_found: bool,
}

pub fn view(cfg: &CalendarConfig) -> CalendarView {
    let r = rt();
    let mut settings = cfg.clone();
    settings.token = String::new();
    settings.pushed.clear();
    CalendarView {
        render: crate::cal_render::status(),
        showing: r.showing.keys().cloned().collect(),
        screensaver_tvs: cfg.screensaver_tvs.clone(),
        outdated: r.outdated.clone(),
        settings,
        saver_found: saver_settings_path().is_file(),
    }
}

/// Small Google displays (Nest Hubs) need big text.
pub fn is_small_display(core: &Core, id: &str) -> bool {
    let inner = core.inner.lock().unwrap();
    inner.devices.get(id).map(|e| e.info.backend == Backend::Cast && e.info.model.to_ascii_lowercase().contains("hub")).unwrap_or(false)
}

pub fn display_settings(cfg: &CalendarConfig, id: &str, small: bool) -> DisplaySettings {
    cfg.displays.get(id).cloned().unwrap_or_else(|| DisplaySettings::default_for(small))
}

fn pick_theme(cfg: &CalendarConfig, wanted: &str) -> String {
    if wanted == "light" || wanted == "dark" { wanted.to_string() } else { cfg.theme.clone() }
}

/// How a screen shows the calendar when shown by hand (or as a screensaver).
pub fn manual_opts(cfg: &CalendarConfig, id: &str, small: bool) -> ShowOpts {
    let d = display_settings(cfg, id, small);
    ShowOpts {
        variant: Variant { theme: pick_theme(cfg, &d.theme), text_pct: d.text_pct.clamp(50, 400), photos: d.photos },
        views: clean_views(&d.views),
        rotate: d.rotate_secs,
    }
}

/// A scheduled time: its own theme and views, the screen's own text size and photos.
fn schedule_opts(cfg: &CalendarConfig, s: &CalSchedule, id: &str, small: bool) -> ShowOpts {
    let d = display_settings(cfg, id, small);
    let theme = if s.theme == "light" || s.theme == "dark" { s.theme.clone() } else { pick_theme(cfg, &d.theme) };
    ShowOpts {
        variant: Variant { theme, text_pct: d.text_pct.clamp(50, 400), photos: d.photos },
        views: clean_views(&s.views),
        rotate: s.rotate_secs,
    }
}

fn clean_views(v: &[String]) -> Vec<String> {
    let out: Vec<String> = VIEWS.iter().filter(|x| v.iter().any(|y| y == *x)).map(|x| x.to_string()).collect();
    if out.is_empty() { vec!["month".into()] } else { out }
}

/// The view a screen shows right now (rotating through its views).
fn current_view(s: &Showing) -> String {
    let n = s.opts.views.len().max(1);
    let i = if s.opts.rotate > 0 && n > 1 { (s.since.elapsed().as_secs() / s.opts.rotate as u64) as usize % n } else { 0 };
    s.opts.views.get(i).cloned().unwrap_or_else(|| "month".into())
}

pub fn ensure_token(core: &Core) -> String {
    let mut inner = core.inner.lock().unwrap();
    if inner.cfg.calendar.token.is_empty() {
        inner.cfg.calendar.token = uuid::Uuid::new_v4().simple().to_string();
        inner.cfg_dirty = true;
    }
    inner.cfg.calendar.token.clone()
}

/// The Roku player's launch parameters for the calendar.
fn roku_params(base: &str, o: &ShowOpts, four_k: bool) -> String {
    format!("cal={}&theme={}&views={}&rotate={}&q={}&every=60",
        crate::backends::roku_player::q(base), o.variant.name(), o.views.join(","), o.rotate, if four_k { "4k" } else { "hd" })
}

// ---------------- showing and stopping ----------------

/// Show the calendar on a TV or Google screen. `save` also keeps it as a Roku screensaver.
pub async fn show_on(core: &Arc<Core>, id: &str, opts: ShowOpts, window: Option<String>, save: bool) -> Result<(), String> {
    let token = ensure_token(core);
    let (backend, four_k) = {
        let inner = core.inner.lock().unwrap();
        let e = inner.devices.get(id).ok_or("Unknown device.")?;
        (e.info.backend, inner.cfg.calendar.four_k)
    };
    match backend {
        Backend::Cast => {
            let ip = core.inner.lock().unwrap().devices.get(id).map(|e| e.info.ip.clone()).unwrap_or_default();
            start_showing(id, &ip, opts, window);
            update_wanted(core);
            update_live(core).await;
            push_pictures(core).await;
            Ok(())
        }
        Backend::Roku => {
            let ip = crate::commands::tv_ready(core, id).await?;
            crate::commands::ensure_player(core, id, &ip).await?;
            start_showing(id, &ip, opts.clone(), window);
            update_wanted(core);
            update_live(core).await;
            let base = crate::media_server::live_url(&token, "", &ip).await?;
            let params = roku_params(&base, &opts, four_k);
            let shown = crate::backends::roku_player::show_calendar(&ip, &params, save).await;
            match &shown {
                Ok(()) if save => {
                    core.inner.lock().unwrap().cfg.calendar.pushed.insert(id.to_string(), params);
                    core.save_config();
                }
                Ok(()) => {}
                Err(_) => {
                    stop_showing(id);
                    update_wanted(core);
                }
            }
            shown
        }
        _ => Err("This device can't show pictures.".into()),
    }
}

fn start_showing(id: &str, ip: &str, opts: ShowOpts, window: Option<String>) {
    let mut r = rt();
    r.elsewhere.remove(id);
    r.showing.insert(id.to_string(), Showing {
        ip: ip.to_string(), since: Instant::now(), touched: Instant::now(), opts, window, pushed: None,
    });
}

/// True if it was showing.
pub fn stop_showing(id: &str) -> bool {
    let mut r = rt();
    r.elsewhere.remove(id);
    r.showing.remove(id).is_some()
}

/// Stop showing on these screens: Google screens stop, Roku TVs leave the player.
pub fn stop(core: &Arc<Core>, ids: &[String]) {
    for id in ids {
        if stop_showing(id) {
            core.send_cmd(id, DeviceCmd::StopCasting);
        }
    }
    update_wanted(core);
}

/// Someone pressed a button on the remote of the TV at `ip` while it showed the calendar.
pub fn touched(ip: std::net::IpAddr) {
    let ip = ip.to_string();
    let mut r = rt();
    for s in r.showing.values_mut().filter(|s| s.ip == ip) {
        s.touched = Instant::now();
    }
}

/// Tell the renderer what to draw.
fn update_wanted(core: &Core) {
    let (saver_variants, four_k_on, roku_ids) = {
        let inner = core.inner.lock().unwrap();
        let c = &inner.cfg.calendar;
        let rokus: HashSet<String> = inner.devices.iter().filter(|(_, e)| e.info.backend == Backend::Roku).map(|(id, _)| id.clone()).collect();
        let saver: Vec<Variant> = c.screensaver_tvs.iter().map(|id| manual_opts(c, id, false).variant).collect();
        (saver, c.four_k, rokus)
    };
    let r = rt();
    let mut variants: BTreeSet<Variant> = r.showing.values().map(|s| s.opts.variant.clone()).collect();
    let screensaver = !saver_variants.is_empty();
    variants.extend(saver_variants);
    let roku_in_use = screensaver || r.showing.keys().any(|id| roku_ids.contains(id));
    drop(r);
    crate::cal_render::want(variants, four_k_on && roku_in_use);
}

/// Only the TVs showing the calendar (or using it as a screensaver) may fetch it.
async fn update_live(core: &Core) {
    let (token, saver_ips) = {
        let inner = core.inner.lock().unwrap();
        let c = &inner.cfg.calendar;
        let ips: Vec<String> = c.screensaver_tvs.iter().filter_map(|id| inner.devices.get(id).map(|e| e.info.ip.clone())).collect();
        (c.token.clone(), ips)
    };
    if token.is_empty() {
        return;
    }
    let mut ips: HashSet<String> = saver_ips.into_iter().collect();
    ips.extend(rt().showing.values().map(|s| s.ip.clone()));
    let allowed: Vec<std::net::IpAddr> = ips.iter().filter_map(|ip| ip.parse().ok()).collect();
    crate::media_server::set_live(&token, &crate::cal_render::dir(), allowed).await;
}

/// Send Google screens their current picture when it's new (or their view rotated).
async fn push_pictures(core: &Core) {
    let token = core.inner.lock().unwrap().cfg.calendar.token.clone();
    let targets: Vec<(String, String, String)> = {
        let inner = core.inner.lock().unwrap();
        let r = rt();
        r.showing.iter()
            .filter(|(id, _)| inner.devices.get(*id).map(|e| e.info.backend == Backend::Cast).unwrap_or(false))
            .map(|(id, s)| (id.clone(), s.ip.clone(), file_name(&s.opts.variant.name(), &current_view(s), "jpg")))
            .collect()
    };
    for (id, ip, name) in targets {
        let Some(mtime) = std::fs::metadata(crate::cal_render::dir().join(&name)).and_then(|m| m.modified()).ok() else { continue };
        // Only pictures made in the last few minutes (not yesterday's).
        if mtime.elapsed().map(|e| e > Duration::from_secs(180)).unwrap_or(true) {
            continue;
        }
        let already = rt().showing.get(&id).and_then(|s| s.pushed.clone()) == Some((name.clone(), mtime));
        if already {
            continue;
        }
        let Ok(url) = crate::media_server::live_url(&token, &name, &ip).await else { continue };
        let version = mtime.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        core.send_cmd(&id, DeviceCmd::Cast(vec![CastItem {
            url: format!("{url}?v={version}"),
            title: "Calendar".into(),
            content_type: "image/jpeg".into(),
            subtitles: None,
            autoplay: true,
        }]));
        if let Some(s) = rt().showing.get_mut(&id) {
            s.pushed = Some((name, mtime));
        }
    }
}

/// Called by the renderer after each round of pictures.
pub async fn pictures_ready(core: &Core) {
    push_pictures(core).await;
}

// ---------------- the regular check ----------------

struct Dev {
    backend: Backend,
    online: bool,
    power: Option<bool>,
    activity: Option<String>,
    tv_showing: Option<String>,
    media_state: Option<String>,
    media_title: Option<String>,
}

fn devices(core: &Core) -> HashMap<String, Dev> {
    let inner = core.inner.lock().unwrap();
    inner.devices.iter().map(|(id, e)| (id.clone(), Dev {
        backend: e.info.backend,
        online: e.info.online,
        power: e.info.tv.as_ref().and_then(|t| t.power),
        activity: e.info.tv.as_ref().and_then(|t| t.activity.clone()),
        tv_showing: e.info.tv.as_ref().and_then(|t| t.showing.clone()),
        media_state: e.info.media.as_ref().map(|m| m.state.clone()),
        media_title: e.info.media.as_ref().and_then(|m| m.title.clone()),
    })).collect()
}

fn on_our_player(d: &Dev) -> bool {
    d.tv_showing.as_deref().map(|s| s.contains("Volume Sync")).unwrap_or(false)
}

/// Watching something we shouldn't cover up (a screensaver or home screen is fine).
fn busy(d: &Dev) -> bool {
    match d.backend {
        Backend::Roku => !on_our_player(d) && matches!(d.activity.as_deref(), Some("playing" | "paused" | "live-tv" | "loading" | "input" | "app")),
        Backend::Cast => d.media_title.as_deref() != Some("Calendar")
            && matches!(d.media_state.as_deref(), Some("PLAYING" | "PAUSED" | "BUFFERING")),
        _ => true,
    }
}

pub async fn run(core: Arc<Core>) {
    import_saver_once(&core);
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tick.tick().await;
        step(&core).await;
    }
}

pub async fn step(core: &Arc<Core>) {
    let devs = devices(core);
    let mut changed = false;

    // 1. Screens that moved on to something else stop getting the calendar.
    {
        let mut r = rt();
        let ids: Vec<(String, Instant)> = r.showing.iter().map(|(k, v)| (k.clone(), v.since)).collect();
        for (id, since) in ids {
            if since.elapsed() < SETTLE {
                continue;
            }
            let elsewhere = match devs.get(&id) {
                None => true,
                Some(d) => match d.backend {
                    Backend::Cast => d.media_title.as_deref().map(|t| t != "Calendar").unwrap_or(false),
                    Backend::Roku => d.power == Some(false) || d.tv_showing.as_deref().map(|_| !on_our_player(d)).unwrap_or(false),
                    _ => true,
                },
            };
            let n = r.elsewhere.entry(id.clone()).or_insert(0);
            *n = if elsewhere { *n + 1 } else { 0 };
            if *n >= ELSEWHERE_LIMIT {
                info!(id=%id, "calendar: screen moved on to something else");
                r.showing.remove(&id);
                r.elsewhere.remove(&id);
                changed = true;
            }
        }
    }

    // 2. Scheduled times (every 15 seconds is plenty).
    let due = rt().last_schedules.map(|t| t.elapsed() >= Duration::from_secs(15)).unwrap_or(true);
    if due {
        rt().last_schedules = Some(Instant::now());
        if run_schedules(core, &devs).await {
            changed = true;
        }
    }

    // 3. Keep the renderer, the picture link and the Google screens in step.
    update_wanted(core);
    update_live(core).await;
    push_pictures(core).await;

    // 4. Screensaver TVs whose saved settings went stale (address, theme or quality changed).
    let (cfg, token) = {
        let inner = core.inner.lock().unwrap();
        (inner.cfg.calendar.clone(), inner.cfg.calendar.token.clone())
    };
    if !token.is_empty() {
        let mut outdated = Vec::new();
        for id in &cfg.screensaver_tvs {
            let ip = core.inner.lock().unwrap().devices.get(id).map(|e| e.info.ip.clone());
            let Some(ip) = ip else { continue };
            if let Ok(base) = crate::media_server::live_url(&token, "", &ip).await {
                if cfg.pushed.get(id) != Some(&roku_params(&base, &manual_opts(&cfg, id, false), cfg.four_k)) {
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

fn parse_hm(s: &str) -> (u32, u32) {
    let mut it = s.split(':').map(|x| x.trim().parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(7).min(23), it.next().unwrap_or(0).min(59))
}

/// Start, watch and end scheduled calendar times. Returns true if anything changed.
async fn run_schedules(core: &Arc<Core>, devs: &HashMap<String, Dev>) -> bool {
    let cfg = core.inner.lock().unwrap().cfg.calendar.clone();
    let now = Local::now();
    let mut changed = false;
    let mut actions: Vec<(String, String, ShowOpts)> = Vec::new(); // (window key, device, opts) to show

    for s in cfg.schedules.iter().filter(|s| s.enabled && !s.devices.is_empty() && s.duration_min > 0) {
        let (h, m) = parse_hm(&s.start);
        for back in [0i64, 1] {
            let date = now.date_naive() - chrono::Duration::days(back);
            if !s.days[date.weekday().num_days_from_monday() as usize] {
                continue;
            }
            let Some(start) = date.and_hms_opt(h, m, 0).and_then(|t| Local.from_local_datetime(&t).earliest()) else { continue };
            let end = start + chrono::Duration::minutes(s.duration_min as i64);
            if now < start || now >= end {
                continue;
            }
            for id in &s.devices {
                let key = format!("{}|{}|{}", s.id, date, id);
                let Some(d) = devs.get(id) else { continue };
                let mut r = rt();
                let showing_here = r.showing.get(id).map(|x| x.window.as_deref() == Some(key.as_str())).unwrap_or(false);
                let touched = r.showing.get(id).map(|x| x.touched.elapsed());
                let w = r.windows.entry(key.clone()).or_insert(Win { end, shown: false, turned_on: false, power_asked: None, done: false });
                if w.done {
                    continue;
                }
                if w.shown {
                    if !showing_here {
                        w.done = true; // someone switched away: leave the TV alone
                        continue;
                    }
                    // Nobody has pressed a button for a while: turn it off early.
                    if s.idle_off_min > 0 && touched.map(|t| t >= Duration::from_secs(s.idle_off_min as u64 * 60)).unwrap_or(false) {
                        w.done = true;
                        drop(r);
                        info!(id=%id, "calendar: nobody used the remote; turning off");
                        finish(core, id, d.backend, true);
                        changed = true;
                    }
                    continue;
                }
                if !d.online && d.backend != Backend::Roku {
                    continue;
                }
                if d.backend == Backend::Roku && d.power != Some(true) {
                    // Off (or unknown): turn it on, then show on a later check.
                    if s.power_on && w.power_asked.map(|t| t.elapsed() > Duration::from_secs(60)).unwrap_or(true) {
                        w.power_asked = Some(Instant::now());
                        w.turned_on = true;
                        drop(r);
                        info!(id=%id, "calendar: turning the TV on for the calendar");
                        core.send_cmd(id, DeviceCmd::Power(true));
                    }
                    continue;
                }
                if s.dont_interrupt && busy(d) {
                    continue; // try again on the next check
                }
                w.shown = true;
                drop(r);
                let small = is_small_display(core, id);
                actions.push((key.clone(), id.clone(), schedule_opts(&cfg, s, id, small)));
            }
        }
    }

    for (key, id, opts) in actions {
        info!(id=%id, "calendar: scheduled time; showing the calendar");
        if let Err(e) = show_on(core, &id, opts, Some(key.clone()), false).await {
            warn!(id=%id, error=%e, "calendar: couldn't show the scheduled calendar");
            if let Some(w) = rt().windows.get_mut(&key) {
                w.shown = false; // try again
            }
        }
        changed = true;
    }

    // Windows that ended: take the calendar down (and the TV off, if chosen).
    let ended: Vec<(String, bool)> = {
        let r = rt();
        r.windows.iter().filter(|(_, w)| !w.done && now >= w.end).map(|(k, w)| (k.clone(), w.shown)).collect()
    };
    for (key, shown) in ended {
        let id = key.rsplit('|').next().unwrap_or("").to_string();
        let sched_id = key.split('|').next().unwrap_or("");
        let off_after = cfg.schedules.iter().find(|s| s.id == sched_id).map(|s| s.off_after).unwrap_or(false);
        let still = rt().showing.get(&id).map(|x| x.window.as_deref() == Some(key.as_str())).unwrap_or(false);
        if let Some(w) = rt().windows.get_mut(&key) {
            w.done = true;
        }
        if shown && still {
            let backend = devs.get(&id).map(|d| d.backend).unwrap_or(Backend::Cast);
            info!(id=%id, off_after, "calendar: scheduled time is over");
            finish(core, &id, backend, off_after);
            changed = true;
        }
    }
    // Forget windows from before yesterday.
    rt().windows.retain(|_, w| now - w.end < chrono::Duration::days(2));
    changed
}

/// Take the calendar off a screen; with `power_off`, turn a Roku TV off too.
fn finish(core: &Arc<Core>, id: &str, backend: Backend, power_off: bool) {
    stop_showing(id);
    if power_off && backend == Backend::Roku {
        core.send_cmd(id, DeviceCmd::Power(false));
    } else {
        core.send_cmd(id, DeviceCmd::StopCasting);
    }
    update_wanted(core);
}

// ---------------- importing from the Calendar Saver ----------------

pub fn saver_settings_path() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("PactoTechCalendarSaver").join("settings.json")
}

/// Copy calendars, photo folders and timings from the PactoTech Calendar Saver.
pub fn import_saver(core: &Core) -> Result<String, String> {
    let text = std::fs::read_to_string(saver_settings_path()).map_err(|_| "The Calendar Saver's settings weren't found on this PC.".to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("Couldn't read the Calendar Saver's settings: {e}"))?;
    let get = |k: &str| v.get(k).or_else(|| v.get(&format!("{}{}", k[..1].to_uppercase(), &k[1..])));
    let (mut feeds_added, mut folders_added) = (0, 0);
    {
        let mut inner = core.inner.lock().unwrap();
        let c = &mut inner.cfg.calendar;
        for f in get("feeds").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            let s = |k: &str| f.get(k).or_else(|| f.get(&format!("{}{}", k[..1].to_uppercase(), &k[1..]))).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let url = s("url");
            if url.is_empty() || c.feeds.iter().any(|x| x.url.trim() == url.trim()) {
                continue;
            }
            let enabled = f.get("enabled").or_else(|| f.get("Enabled")).and_then(|x| x.as_bool()).unwrap_or(true);
            let color = s("color");
            c.feeds.push(crate::cal_feeds::FeedCfg { url, name: s("name"), color: if color.is_empty() { "#7aa2f7".into() } else { color }, enabled });
            feeds_added += 1;
        }
        for p in get("photoFolders").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            if let Some(p) = p.as_str() {
                if !c.photo_folders.iter().any(|x| x.eq_ignore_ascii_case(p)) {
                    c.photo_folders.push(p.to_string());
                    folders_added += 1;
                }
            }
        }
        if let Some(n) = get("photoIntervalSeconds").and_then(|x| x.as_u64()) {
            c.photo_interval_secs = (n as u32).clamp(5, 3600);
        }
        if let Some(n) = get("refreshMinutes").and_then(|x| x.as_u64()) {
            c.refresh_minutes = (n as u32).clamp(1, 1440);
        }
        c.saver_imported = true;
    }
    core.save_config();
    crate::cal_render::kick(true);
    info!(feeds_added, folders_added, "calendar: imported from the Calendar Saver");
    Ok(format!("Copied {feeds_added} calendar{} and {folders_added} photo folder{} from the Calendar Saver.",
        if feeds_added == 1 { "" } else { "s" }, if folders_added == 1 { "" } else { "s" }))
}

/// First run: bring over the saver's calendars if this app has none yet.
fn import_saver_once(core: &Core) {
    let (empty, done) = {
        let inner = core.inner.lock().unwrap();
        (inner.cfg.calendar.feeds.is_empty(), inner.cfg.calendar.saver_imported)
    };
    if empty && !done && saver_settings_path().is_file() {
        let _ = import_saver(core);
    }
}
