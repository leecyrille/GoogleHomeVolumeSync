//! Yamaha MusicCast backend via Yamaha Extended Control (YXC) JSON HTTP API.
//! Absolute volume supported (0..max_volume steps mapped to 0..100%).

use crate::types::{CoreEvent, DeviceCmd, MediaInfo};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub struct YamahaActor {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub cmd_rx: mpsc::Receiver<DeviceCmd>,
    pub events: mpsc::Sender<CoreEvent>,
}

impl YamahaActor {
    pub async fn run(mut self) {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(4)).build().unwrap();
        let base = format!("http://{}/YamahaExtendedControl/v1", self.ip);
        let mut poll = tokio::time::interval(Duration::from_secs(5));
        let mut online = false;
        let mut max_volume: f32 = 161.0; // RX-V581 default; refreshed from getStatus

        loop {
            tokio::select! {
                _ = poll.tick() => {
                    match get_json(&client, &format!("{base}/main/getStatus")).await {
                        Some(status) => {
                            if !online {
                                online = true;
                                info!(id=%self.id, name=%self.name, "yamaha: online");
                                let _ = self.events.send(CoreEvent::Online { id: self.id.clone(), online: true }).await;
                                if let Some(feat) = get_json(&client, &format!("{base}/system/getFeatures")).await {
                                    if let Some(mv) = feat["zone"].as_array()
                                        .and_then(|zs| zs.iter().find(|z| z["id"] == "main"))
                                        .and_then(|z| z["range_step"].as_array())
                                        .and_then(|rs| rs.iter().find(|r| r["id"] == "volume"))
                                        .and_then(|r| r["max"].as_f64()) {
                                        max_volume = mv as f32;
                                        info!(id=%self.id, max_volume, "yamaha: max volume");
                                    }
                                }
                            }
                            let vol = status["volume"].as_f64().unwrap_or(0.0) as f32;
                            let muted = status["mute"].as_bool().unwrap_or(false);
                            let _ = self.events.send(CoreEvent::VolumeChanged {
                                id: self.id.clone(), volume: (vol / max_volume).clamp(0.0, 1.0), muted,
                            }).await;
                            if let Some(pb) = get_json(&client, &format!("{base}/netusb/getPlayInfo")).await {
                                let state = pb["playback"].as_str().unwrap_or("stop");
                                let media = if state == "play" || state == "pause" {
                                    Some(MediaInfo {
                                        state: if state == "play" { "PLAYING".into() } else { "PAUSED".into() },
                                        title: pb["track"].as_str().filter(|s| !s.is_empty()).map(String::from),
                                        artist: pb["artist"].as_str().filter(|s| !s.is_empty()).map(String::from),
                                        app: pb["input"].as_str().map(String::from),
                                        supports_transport: true,
                                        album: pb["album"].as_str().filter(|s| !s.is_empty()).map(String::from),
                                        // albumart_url is a path on the receiver itself.
                                        image: pb["albumart_url"].as_str()
                                            .filter(|s| !s.is_empty())
                                            .map(|p| if p.starts_with("http") { p.to_string() } else { format!("http://{}{}", self.ip, p) }),
                                    })
                                } else { None };
                                let _ = self.events.send(CoreEvent::MediaChanged { id: self.id.clone(), media }).await;
                            }
                        }
                        None => {
                            if online {
                                online = false;
                                warn!(id=%self.id, name=%self.name, "yamaha: offline");
                                let _ = self.events.send(CoreEvent::Online { id: self.id.clone(), online: false }).await;
                            }
                        }
                    }
                }
                cmd = self.cmd_rx.recv() => {
                    let cmd = match cmd { Some(c) => c, None => return };
                    if matches!(cmd, DeviceCmd::Shutdown) { return; }
                    if matches!(cmd, DeviceCmd::Power(_) | DeviceCmd::Input(_) | DeviceCmd::Key(_) | DeviceCmd::Seek(_) | DeviceCmd::Resync) { continue; }
                    info!(id=%self.id, name=%self.name, ?cmd, "yamaha: sending command");
                    let url = match cmd {
                        DeviceCmd::SetVolume(level) => {
                            let steps = (level * max_volume).round() as i32;
                            format!("{base}/main/setVolume?volume={steps}")
                        }
                        DeviceCmd::SetMuted(m) => format!("{base}/main/setMute?enable={m}"),
                        DeviceCmd::Play => format!("{base}/netusb/setPlayback?playback=play"),
                        DeviceCmd::Pause => format!("{base}/netusb/setPlayback?playback=pause"),
                        DeviceCmd::Next => format!("{base}/netusb/setPlayback?playback=next"),
                        DeviceCmd::Prev => format!("{base}/netusb/setPlayback?playback=previous"),
                        DeviceCmd::Refresh => format!("{base}/main/getStatus"),
                        DeviceCmd::Shutdown | DeviceCmd::Power(_) | DeviceCmd::Input(_) | DeviceCmd::Key(_) | DeviceCmd::Seek(_) | DeviceCmd::Resync => unreachable!(),
                    };
                    let _ = get_json(&client, &url).await;
                }
            }
        }
    }
}

async fn get_json(client: &reqwest::Client, url: &str) -> Option<Value> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().await.ok()
}
