//! Roku TV backend via ECP (HTTP on port 8060).
//!
//! Roku ECP has no absolute volume: we emulate it. To set N%, either apply a
//! delta from the last known (cached) level, or — when confidence is lost —
//! re-zero by sending VolumeDown ~102 times, then VolumeUp N times.
//! The cached level is reported via VolumeChanged so the core can persist it.

//! Power, inputs and the remote use the same documented protocol: keypresses
//! (PowerOn/PowerOff, InputHDMI1.., Home, Up..), /query/device-info for the
//! power mode and MAC addresses, /query/active-app for what's on screen, and
//! /query/apps + /launch for named inputs and channels. TVs whose "Control by
//! mobile apps" setting is Limited refuse /query/apps, so a fixed input list
//! is used there.

use crate::types::{CoreEvent, DeviceCmd, InputOption, MediaInfo, TvStatus};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

pub struct RokuActor {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub cmd_rx: mpsc::Receiver<DeviceCmd>,
    pub events: mpsc::Sender<CoreEvent>,
    /// Last known volume 0..100, or None if unknown (forces re-zero).
    pub cached_level: Option<u8>,
    /// Known MAC addresses, for waking a TV that no longer answers on the network.
    pub macs: Vec<String>,
}

// TCL/Roku TVs silently drop volume keypresses sent faster than ~3-4/s.
const KEY_DELAY: Duration = Duration::from_millis(300);
const ZERO_SETTLE: Duration = Duration::from_millis(1000);

/// Remote keys the UI may send. Volume keys are excluded: the slider owns volume
/// so the cached level stays correct.
const REMOTE_KEYS: &[&str] = &[
    "Home", "Back", "Select", "Up", "Down", "Left", "Right", "Info", "InstantReplay",
    "Play", "Rev", "Fwd", "ChannelUp", "ChannelDown", "Search",
];

/// Roku's own screensaver / ambient apps. The TV reports them as ordinary apps,
/// but they only run when nobody is watching. (773622 = Backdrops, 123095 = Aquatic Life)
const SCREENSAVER_APPS: &[&str] = &["773622", "123095"];

fn is_screensaver_app(id: &str, name: &str) -> bool {
    let n = name.to_lowercase();
    SCREENSAVER_APPS.contains(&id) || n.contains("screensaver") || n.contains("screen saver") || n == "roku city"
}

/// Used when the TV won't list its inputs (Limited mode).
const FALLBACK_INPUTS: &[(&str, &str)] = &[
    ("key:InputTuner", "Live TV"),
    ("key:InputHDMI1", "HDMI 1"),
    ("key:InputHDMI2", "HDMI 2"),
    ("key:InputHDMI3", "HDMI 3"),
    ("key:InputHDMI4", "HDMI 4"),
    ("key:InputAV1", "AV"),
];

/// What was last reported, so events only go out on change.
#[derive(Default)]
struct Seen {
    tv: TvStatus,
    macs: Vec<String>,
    media: Option<MediaInfo>,
    /// The input/app list changes rarely; it's re-read every minute.
    inputs: Vec<InputOption>,
    inputs_at: Option<std::time::Instant>,
}

/// Poll quickly while something plays so the progress bar stays accurate.
const POLL_PLAYING: Duration = Duration::from_secs(3);
const POLL_IDLE: Duration = Duration::from_secs(10);

impl RokuActor {
    pub async fn run(mut self) {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(4))
            .build()
            .unwrap();
        let base = format!("http://{}:8060", self.ip);
        let mut next_poll = tokio::time::Instant::now();
        let mut online = false;
        let mut last = Seen::default();

        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(next_poll) => {
                    self.poll_status(&client, &base, &mut online, &mut last).await;
                    let playing = matches!(last.tv.activity.as_deref(), Some("playing" | "loading"));
                    next_poll = tokio::time::Instant::now() + if playing { POLL_PLAYING } else { POLL_IDLE };
                }
                cmd = self.cmd_rx.recv() => {
                    let cmd = match cmd { Some(c) => c, None => return };
                    match cmd {
                        DeviceCmd::Shutdown => return,
                        DeviceCmd::SetVolume(level) => {
                            // Collapse any queued-up SetVolume commands (slider drags):
                            // only the most recent target matters, ramps are slow.
                            let mut level = level;
                            while let Ok(next) = self.cmd_rx.try_recv() {
                                match next {
                                    DeviceCmd::SetVolume(l) => level = l,
                                    DeviceCmd::Shutdown => return,
                                    _ => {}
                                }
                            }
                            let target = (level * 100.0).round().clamp(0.0, 100.0) as i32;
                            self.set_volume(&client, &base, target).await;
                        }
                        DeviceCmd::SetMuted(_) => {
                            // ECP mute is a toggle; send VolumeMute.
                            let _ = keypress(&client, &base, "VolumeMute").await;
                            info!(id=%self.id, "roku: VolumeMute toggled");
                        }
                        DeviceCmd::Play | DeviceCmd::Pause => { let _ = keypress(&client, &base, "Play").await; }
                        DeviceCmd::Next => { let _ = keypress(&client, &base, "Fwd").await; }
                        DeviceCmd::Prev => { let _ = keypress(&client, &base, "Rev").await; }
                        DeviceCmd::Refresh => {
                            // Force recalibration next set.
                            self.cached_level = None;
                            info!(id=%self.id, "roku: cached level cleared (recalibrate on next set)");
                        }
                        DeviceCmd::Power(on) => {
                            self.set_power(&client, &base, on).await;
                            tokio::time::sleep(Duration::from_millis(1500)).await;
                            self.poll_status(&client, &base, &mut online, &mut last).await;
                        }
                        DeviceCmd::Input(input) => {
                            info!(id=%self.id, name=%self.name, input=%input, "roku: switch input");
                            let ok = if let Some(key) = input.strip_prefix("key:") {
                                match keypress(&client, &base, key).await {
                                    Ok(()) => true,
                                    Err(e) => { warn!(id=%self.id, error=?e, "roku: input key rejected"); false }
                                }
                            } else if let Some(app) = input.strip_prefix("app:") {
                                client.post(format!("{base}/launch/{app}")).send().await
                                    .map(|r| r.status().is_success()).unwrap_or(false)
                            } else {
                                false
                            };
                            if !ok {
                                warn!(id=%self.id, input=%input, "roku: input switch failed");
                            }
                            tokio::time::sleep(Duration::from_millis(1500)).await;
                            self.poll_status(&client, &base, &mut online, &mut last).await;
                        }
                        DeviceCmd::Cast(_) => {}
                        DeviceCmd::Resync => {
                            last.inputs_at = None;
                            self.poll_status(&client, &base, &mut online, &mut last).await;
                        }
                        DeviceCmd::Seek(target) => {
                            self.seek(&client, &base, target).await;
                            self.poll_status(&client, &base, &mut online, &mut last).await;
                        }
                        DeviceCmd::Key(key) => {
                            if REMOTE_KEYS.contains(&key.as_str()) {
                                debug!(id=%self.id, key=%key, "roku: remote key");
                                if keypress(&client, &base, &key).await.is_err() {
                                    warn!(id=%self.id, key=%key, "roku: remote key failed");
                                }
                            } else {
                                warn!(id=%self.id, key=%key, "roku: remote key not allowed");
                            }
                        }
                    }
                }
            }
        }
    }

    async fn poll_status(&mut self, client: &reqwest::Client, base: &str, online: &mut bool, last: &mut Seen) {
        let info = get_text(client, &format!("{base}/query/device-info")).await;
        let ok = info.is_some();
        if ok != *online {
            *online = ok;
            info!(id=%self.id, name=%self.name, online = ok, "roku: reachability changed");
            let _ = self.events.send(CoreEvent::Online { id: self.id.clone(), online: ok }).await;
        }
        let Some(info) = info else { return };

        let is_tv = xml_tag(&info, "is-tv").as_deref() == Some("true");
        for m in [xml_tag(&info, "wifi-mac"), xml_tag(&info, "ethernet-mac")].into_iter().flatten() {
            if !m.is_empty() && !self.macs.contains(&m) {
                self.macs.push(m);
            }
        }
        let model = match (xml_tag(&info, "vendor-name"), xml_tag(&info, "model-name")) {
            (Some(v), Some(m)) if !v.is_empty() => Some(format!("{v} {m}")),
            (_, m) => m,
        };

        // Limited mode refuses the media-player query with a 403 / explanatory text.
        let player = match client.get(format!("{base}/query/media-player")).send().await {
            Ok(r) => {
                let code = r.status().as_u16();
                let text = r.text().await.unwrap_or_default();
                if code == 403 || text.contains("Limited mode") { Err(()) } else { Ok(text) }
            }
            Err(_) => Ok(String::new()),
        };
        let restricted = player.is_err();

        let stale = last.inputs_at.map_or(true, |t| t.elapsed() > Duration::from_secs(60));
        if is_tv && stale {
            last.inputs = self.list_inputs(client, base).await;
            last.inputs_at = Some(std::time::Instant::now());
        }
        let inputs = if is_tv { last.inputs.clone() } else { Vec::new() };
        let active = get_text(client, &format!("{base}/query/active-app")).await.unwrap_or_default();
        let (active_id, active_kind) = active.split("<app").nth(1)
            .and_then(|c| c.split_once('>'))
            .map(|(attrs, _)| (xml_attr(attrs, "id").unwrap_or_default(), xml_attr(attrs, "type").unwrap_or_default()))
            .unwrap_or_default();
        let showing = active_label(&active, &inputs);
        let showing_icon = (!active_id.is_empty() && active_kind != "home")
            .then(|| format!("http://{}:8060/query/icon/{}", self.ip, active_id));

        // Playback inside an app: "play" / "pause" (the TV UI and HDMI inputs report "close").
        let player_state = player.as_ref().ok()
            .and_then(|x| x.split("<player").nth(1))
            .and_then(|c| c.split_once('>'))
            .and_then(|(attrs, _)| xml_attr(attrs, "state"))
            .unwrap_or_default();
        let app_playing = active_kind == "appl" && (player_state == "play" || player_state == "pause");

        let showing_detail = if active_id == "tvinput.dtv" {
            get_text(client, &format!("{base}/query/tv-active-channel")).await.and_then(|x| {
                let num = xml_tag(&x, "number").filter(|v| !v.is_empty());
                let name = xml_tag(&x, "name").filter(|v| !v.is_empty());
                let program = xml_tag(&x, "program-title").filter(|v| !v.is_empty());
                let channel = [num, name].into_iter().flatten().collect::<Vec<_>>().join(" ");
                let parts: Vec<String> = [Some(channel).filter(|c| !c.is_empty()), program].into_iter().flatten().collect();
                (!parts.is_empty()).then(|| parts.join(" · "))
            })
        } else if app_playing {
            Some(if player_state == "play" { "Playing".into() } else { "Paused".into() })
        } else {
            None
        };

        let power = if is_tv { xml_tag(&info, "power-mode").map(|p| p == "PowerOn") } else { None };
        // Some apps (ambient video like fireplaces) play without the player
        // reporting it, so an app with no reported playback is "app", not idle.
        let activity = if power == Some(false) {
            "off"
        } else if active.contains("<screensaver") || active_kind == "ssvr"
            || (player_state != "play" && is_screensaver_app(&active_id, showing.as_deref().unwrap_or(""))) {
            "screensaver"
        } else if active_kind == "home" {
            "home"
        } else if active_id == "tvinput.dtv" {
            "live-tv"
        } else if active_kind == "tvin" {
            "input"
        } else if player_state == "play" {
            "playing"
        } else if player_state == "pause" {
            "paused"
        } else if player_state == "buffer" || player_state == "startup" {
            "loading"
        } else if active_kind == "appl" {
            "app"
        } else {
            ""
        };

        let (position_ms, duration_ms, is_live) = match player.as_ref() {
            Ok(x) if app_playing => (
                xml_tag(x, "position").and_then(|v| parse_ms(&v)),
                xml_tag(x, "duration").and_then(|v| parse_ms(&v)),
                xml_tag(x, "is_live").as_deref() == Some("true"),
            ),
            _ => (None, None, false),
        };

        let tv = TvStatus {
            restricted,
            has_power: is_tv,
            dev_mode: xml_tag(&info, "developer-enabled").as_deref() == Some("true"),
            player_ready: last.inputs.iter().any(|i| i.id == "app:dev") || active_id == "dev",
            position_ms,
            duration_ms,
            position_at: position_ms.map(|_| unix_ms()),
            is_live,
            activity: (!activity.is_empty()).then(|| activity.to_string()),
            // PowerOn = screen on. Ready / DisplayOff / Headless / Suspend all mean off.
            power,
            showing,
            showing_icon: showing_icon.clone(),
            showing_detail,
            inputs,
            headphones: xml_tag(&info, "headphones-connected").as_deref() == Some("true"),
            model,
            firmware: xml_tag(&info, "software-version").map(|v| format!("Roku OS {v}")),
        };
        // Position moves every poll while playing; only log real changes.
        let quiet = |t: &TvStatus| TvStatus { position_ms: None, position_at: None, ..t.clone() };
        if quiet(&tv) != quiet(&last.tv) {
            info!(id=%self.id, name=%self.name, power=?tv.power, activity=?tv.activity, showing=?tv.showing, detail=?tv.showing_detail,
                inputs=tv.inputs.len(), restricted=tv.restricted, headphones=tv.headphones, "roku: status");
        }
        if tv != last.tv || self.macs != last.macs {
            last.tv = tv.clone();
            last.macs = self.macs.clone();
            let _ = self.events.send(CoreEvent::DeviceStatus { id: self.id.clone(), tv, macs: self.macs.clone() }).await;
        }

        // A streaming app that's playing counts as a media session, so it
        // shows in Now Playing with transport controls.
        let media = app_playing.then(|| MediaInfo {
            state: if player_state == "play" { "PLAYING".into() } else { "PAUSED".into() },
            title: None,
            artist: None,
            app: last.tv.showing.clone(),
            supports_transport: true,
            album: None,
            image: showing_icon,
            ..Default::default()
        });
        let changed = match (&media, &last.media) {
            (Some(a), Some(b)) => a.state != b.state || a.app != b.app,
            (None, None) => false,
            _ => true,
        };
        if changed {
            last.media = media.clone();
            let _ = self.events.send(CoreEvent::MediaChanged { id: self.id.clone(), media }).await;
        }
    }

    /// Named inputs and apps from /query/apps when the TV allows it,
    /// otherwise the fixed input-key list.
    async fn list_inputs(&self, client: &reqwest::Client, base: &str) -> Vec<InputOption> {
        let apps = get_text(client, &format!("{base}/query/apps")).await.unwrap_or_default();
        let parsed = parse_apps(&apps);
        if parsed.is_empty() {
            return FALLBACK_INPUTS.iter()
                .map(|(id, label)| InputOption { id: id.to_string(), label: label.to_string(), kind: "input".into(), icon: None })
                .collect();
        }
        // TV inputs first, in the TV's order, then apps.
        let (tvin, apps): (Vec<_>, Vec<_>) = parsed.into_iter().partition(|(_, kind, _)| kind == "tvin");
        let icon = |id: &str| Some(format!("http://{}:8060/query/icon/{}", self.ip, id));
        // Our own player channel ("dev") is kept with kind "player" so it can be
        // detected, and hidden from the Switch to list in the UI.
        tvin.iter().map(|(id, _, name)| InputOption { id: format!("app:{id}"), label: name.clone(), kind: "input".into(), icon: icon(id) })
            .chain(apps.iter().map(|(id, _, name)| InputOption {
                id: format!("app:{id}"), label: name.clone(),
                kind: if id == "dev" { "player".into() } else { "app".into() }, icon: icon(id),
            }))
            .collect()
    }

    /// ECP has no absolute seek, so scan with Fwd/Rev while watching the
    /// position, then press Play near the target. Lands close, not exact, and
    /// depends on the app reporting its position while scanning.
    async fn seek(&self, client: &reqwest::Client, base: &str, target: u64) {
        let read = || async {
            let x = get_text(client, &format!("{base}/query/media-player")).await?;
            xml_tag(&x, "position").and_then(|v| parse_ms(&v))
        };
        // Our own player channel takes an exact position.
        let active = get_text(client, &format!("{base}/query/active-app")).await.unwrap_or_default();
        if active.contains(r#"id="dev""#) {
            let _ = client.post(format!("{base}/input?seek={target}")).send().await;
            info!(id=%self.id, target, "roku: seek (player channel)");
            return;
        }
        let Some(start) = read().await else {
            warn!(id=%self.id, "roku: seek: app isn't reporting a position");
            return;
        };
        let forward = target > start;
        let distance = target.abs_diff(start);
        if distance < 4_000 {
            return;
        }
        info!(id=%self.id, start, target, "roku: seek");
        // Each press steps the scan speed up; go faster for long jumps.
        let presses = if distance > 600_000 { 3 } else if distance > 120_000 { 2 } else { 1 };
        let key = if forward { "Fwd" } else { "Rev" };
        for _ in 0..presses {
            if keypress(client, base, key).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        let begun = std::time::Instant::now();
        let mut prev = (start, std::time::Instant::now());
        let mut still_since = std::time::Instant::now();
        loop {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let Some(pos) = read().await else { break };
            let now = std::time::Instant::now();
            // Scan rate (ms of video per ms of wall time) to lead the Play press.
            let rate = pos.abs_diff(prev.0) as f64 / now.duration_since(prev.1).as_millis().max(1) as f64;
            if pos != prev.0 {
                still_since = now;
            }
            prev = (pos, now);
            let lead = (rate * 450.0) as u64 + 1_000;
            let arrived = if forward { pos + lead >= target } else { pos <= target + lead };
            if arrived {
                break;
            }
            if now.duration_since(still_since) > Duration::from_secs(4) {
                warn!(id=%self.id, "roku: seek: position isn't moving while scanning; stopping");
                break;
            }
            if begun.elapsed() > Duration::from_secs(90) {
                warn!(id=%self.id, "roku: seek: gave up after 90 s");
                break;
            }
        }
        let _ = keypress(client, base, "Play").await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        let landed = read().await;
        info!(id=%self.id, target, landed=?landed, "roku: seek done");
    }

    async fn set_power(&self, client: &reqwest::Client, base: &str, on: bool) {
        info!(id=%self.id, name=%self.name, on, "roku: set power");
        if !on {
            if keypress(client, base, "PowerOff").await.is_err() {
                warn!(id=%self.id, "roku: PowerOff failed");
            }
            return;
        }
        match keypress(client, base, "PowerOn").await {
            Ok(()) => return,
            Err(KeyError::Refused(code)) => {
                warn!(id=%self.id, code, "roku: TV refused PowerOn (Control by mobile apps is probably Limited)");
                return;
            }
            Err(KeyError::Unreachable) => {}
        }
        // Not answering: it's in deep sleep. Wake it over the network, then retry.
        if self.macs.is_empty() {
            warn!(id=%self.id, "roku: unreachable and no MAC known, can't wake it");
            return;
        }
        for attempt in 1..=6 {
            for mac in &self.macs {
                if let Err(e) = send_wake_on_lan(mac).await {
                    warn!(id=%self.id, mac=%mac, error=%e, "roku: wake-on-LAN send failed");
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            if keypress(client, base, "PowerOn").await.is_ok() {
                info!(id=%self.id, attempt, "roku: woke via wake-on-LAN");
                return;
            }
        }
        warn!(id=%self.id, "roku: still unreachable after wake-on-LAN");
    }

    async fn set_volume(&mut self, client: &reqwest::Client, base: &str, target: i32) {
        info!(id=%self.id, name=%self.name, target, cached=?self.cached_level, "roku: set volume");
        match self.cached_level {
            Some(cur) => {
                let delta = target - cur as i32;
                let key = if delta >= 0 { "VolumeUp" } else { "VolumeDown" };
                for _ in 0..delta.abs() {
                    if keypress(client, base, key).await.is_err() {
                        warn!(id=%self.id, "roku: keypress failed mid-ramp; clearing cache");
                        self.cached_level = None;
                        return;
                    }
                    tokio::time::sleep(KEY_DELAY).await;
                }
            }
            None => {
                // Re-zero: 102 downs guarantees floor on a 0-100 TV.
                debug!(id=%self.id, "roku: re-zeroing volume");
                for _ in 0..102 {
                    if keypress(client, base, "VolumeDown").await.is_err() {
                        warn!(id=%self.id, "roku: keypress failed during re-zero");
                        return;
                    }
                    tokio::time::sleep(KEY_DELAY).await;
                }
                tokio::time::sleep(ZERO_SETTLE).await;
                for _ in 0..target {
                    if keypress(client, base, "VolumeUp").await.is_err() {
                        warn!(id=%self.id, "roku: keypress failed during ramp-up");
                        return;
                    }
                    tokio::time::sleep(KEY_DELAY).await;
                }
            }
        }
        self.cached_level = Some(target.clamp(0, 100) as u8);
        let _ = self.events.send(CoreEvent::VolumeChanged {
            id: self.id.clone(),
            volume: target as f32 / 100.0,
            muted: false,
        }).await;
    }
}

#[derive(Debug)]
enum KeyError {
    /// No answer: the TV is off the network or asleep.
    Unreachable,
    /// The TV answered and said no, e.g. 403 when "Control by mobile apps" is Limited.
    Refused(u16),
}

async fn keypress(client: &reqwest::Client, base: &str, key: &str) -> Result<(), KeyError> {
    let resp = client
        .post(format!("{base}/keypress/{key}"))
        .send()
        .await
        .map_err(|_| KeyError::Unreachable)?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(KeyError::Refused(resp.status().as_u16()))
    }
}

/// Discover Rokus: SSDP M-SEARCH first; if the multicast reply path is blocked
/// (common with the Windows firewall), fall back to probing the local /24 on
/// port 8060. Returns (ip, name, model, serial).
pub async fn discover() -> Vec<(String, String, String, String)> {
    let mut found = ssdp_discover().await;
    if found.is_empty() {
        info!("roku: SSDP found nothing, probing subnet on port 8060");
        found = subnet_probe().await;
    }
    found
}

async fn subnet_probe() -> Vec<(String, String, String, String)> {
    let Some(local) = local_ipv4().await else { return vec![] };
    let octets = local.octets();
    let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for host in 1..=254u8 {
        let ip = std::net::Ipv4Addr::new(octets[0], octets[1], octets[2], host);
        tasks.spawn(async move {
            let ok = tokio::time::timeout(
                Duration::from_millis(400),
                tokio::net::TcpStream::connect((ip, 8060)),
            ).await.map(|r| r.is_ok()).unwrap_or(false);
            ok.then(|| ip.to_string())
        });
    }
    let mut ips = Vec::new();
    while let Some(res) = tasks.join_next().await {
        if let Ok(Some(ip)) = res {
            ips.push(ip);
        }
    }
    let mut found = Vec::new();
    for ip in ips {
        if let Some(info) = query_device_info(&client, &ip).await {
            found.push(info);
        }
    }
    found
}

async fn local_ipv4() -> Option<std::net::Ipv4Addr> {
    // Route trick: connecting a UDP socket picks the outbound interface.
    let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok()?;
    sock.connect("8.8.8.8:80").await.ok()?;
    match sock.local_addr().ok()? {
        std::net::SocketAddr::V4(a) => Some(*a.ip()),
        _ => None,
    }
}

async fn query_device_info(client: &reqwest::Client, ip: &str) -> Option<(String, String, String, String)> {
    let resp = client.get(format!("http://{ip}:8060/query/device-info")).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let xml = resp.text().await.ok()?;
    let tag = |t: &str| xml_tag(&xml, t).unwrap_or_default();
    let name = {
        let n = tag("user-device-name");
        if n.is_empty() { tag("friendly-device-name") } else { n }
    };
    let model = tag("model-name");
    let serial = tag("serial-number");
    info!(ip=%ip, name=%name, model=%model, serial=%serial, "roku: found device");
    Some((ip.to_string(), name, model, serial))
}

/// One-shot SSDP M-SEARCH for Roku devices; returns (ip, name, model, serial).
async fn ssdp_discover() -> Vec<(String, String, String, String)> {
    let mut found = Vec::new();
    let sock = match tokio::net::UdpSocket::bind(("0.0.0.0", 0)).await {
        Ok(s) => s,
        Err(_) => return found,
    };
    let msearch = "M-SEARCH * HTTP/1.1\r\nHost: 239.255.255.250:1900\r\nMan: \"ssdp:discover\"\r\nST: roku:ecp\r\nMX: 3\r\n\r\n";
    let _ = sock.send_to(msearch.as_bytes(), ("239.255.255.250", 1900)).await;
    let client = reqwest::Client::builder().timeout(Duration::from_secs(3)).build().unwrap();
    let mut buf = [0u8; 2048];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    let mut ips: Vec<String> = Vec::new();
    while let Ok(Ok((n, _))) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await {
        let text = String::from_utf8_lossy(&buf[..n]);
        for line in text.lines() {
            if let Some(loc) = line.strip_prefix("LOCATION: ").or_else(|| line.strip_prefix("Location: ")) {
                // e.g. http://192.168.1.50:8060/
                if let Some(host) = loc.trim().strip_prefix("http://").and_then(|r| r.split(':').next()) {
                    if !ips.contains(&host.to_string()) {
                        ips.push(host.to_string());
                    }
                }
            }
        }
    }
    for ip in ips {
        if let Some(info) = query_device_info(&client, &ip).await {
            found.push(info);
        }
    }
    found
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = xml[start..].find(&format!("</{tag}>"))? + start;
    Some(xml_unescape(xml[start..end].trim()))
}

fn xml_attr(element: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let start = element.find(&key)? + key.len();
    let end = element[start..].find('"')? + start;
    Some(xml_unescape(&element[start..end]))
}

/// `<app id="tvinput.hdmi1" type="tvin" version="1.0.0">PS5</app>` -> (id, type, name)
fn parse_apps(xml: &str) -> Vec<(String, String, String)> {
    xml.split("<app ").skip(1).filter_map(|chunk| {
        let (attrs, rest) = chunk.split_once('>')?;
        let name = rest.split("</app>").next()?.trim();
        let id = xml_attr(attrs, "id")?;
        let kind = xml_attr(attrs, "type").unwrap_or_default();
        Some((id, kind, xml_unescape(name)))
    }).collect()
}

/// What's on screen, e.g. "HDMI 2", "Live TV", "Home" or an app name.
fn active_label(xml: &str, inputs: &[InputOption]) -> Option<String> {
    let chunk = xml.split("<app").nth(1)?;
    let (attrs, rest) = chunk.split_once('>')?;
    let name = xml_unescape(rest.split("</app>").next().unwrap_or("").trim());
    let id = xml_attr(attrs, "id").unwrap_or_default();
    if xml_attr(attrs, "type").as_deref() == Some("home") || id.is_empty() {
        return Some("Home".into());
    }
    if let Some(named) = inputs.iter().find(|i| i.id == format!("app:{id}")) {
        return Some(named.label.clone());
    }
    let generic = match id.as_str() {
        "tvinput.dtv" => Some("Live TV".to_string()),
        "tvinput.cvbs" => Some("AV".to_string()),
        other => other.strip_prefix("tvinput.hdmi").map(|n| format!("HDMI {n}")),
    };
    generic.or(if name.is_empty() { None } else { Some(name) })
}

/// Standard magic packet: 6 x 0xFF followed by the MAC repeated 16 times.
async fn send_wake_on_lan(mac: &str) -> std::io::Result<()> {
    let bytes: Vec<u8> = mac.split([':', '-'])
        .filter_map(|b| u8::from_str_radix(b, 16).ok())
        .collect();
    if bytes.len() != 6 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad MAC address"));
    }
    let mut packet = vec![0xFFu8; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&bytes);
    }
    let sock = tokio::net::UdpSocket::bind(("0.0.0.0", 0)).await?;
    sock.set_broadcast(true)?;
    for port in [9u16, 7] {
        sock.send_to(&packet, ("255.255.255.255", port)).await?;
    }
    Ok(())
}

async fn get_text(client: &reqwest::Client, url: &str) -> Option<String> {
    match client.get(url).send().await {
        Ok(r) if r.status().is_success() => r.text().await.ok(),
        _ => None,
    }
}

/// "12345 ms" -> 12345
fn parse_ms(v: &str) -> Option<u64> {
    v.split_whitespace().next()?.parse().ok()
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
