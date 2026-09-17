//! Roku TV backend via ECP (HTTP on port 8060).
//!
//! Roku ECP has no absolute volume: we emulate it. To set N%, either apply a
//! delta from the last known (cached) level, or — when confidence is lost —
//! re-zero by sending VolumeDown ~102 times, then VolumeUp N times.
//! The cached level is reported via VolumeChanged so the core can persist it.

use crate::types::{CoreEvent, DeviceCmd};
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
}

const KEY_DELAY: Duration = Duration::from_millis(120);

impl RokuActor {
    pub async fn run(mut self) {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(4))
            .build()
            .unwrap();
        let base = format!("http://{}:8060", self.ip);
        let mut poll = tokio::time::interval(Duration::from_secs(15));
        let mut online = false;

        loop {
            tokio::select! {
                _ = poll.tick() => {
                    let ok = client.get(format!("{base}/query/device-info")).send().await
                        .map(|r| r.status().is_success()).unwrap_or(false);
                    if ok != online {
                        online = ok;
                        info!(id=%self.id, name=%self.name, online, "roku: reachability changed");
                        let _ = self.events.send(CoreEvent::Online { id: self.id.clone(), online }).await;
                    }
                }
                cmd = self.cmd_rx.recv() => {
                    let cmd = match cmd { Some(c) => c, None => return };
                    match cmd {
                        DeviceCmd::Shutdown => return,
                        DeviceCmd::SetVolume(level) => {
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
                    }
                }
            }
        }
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

async fn keypress(client: &reqwest::Client, base: &str, key: &str) -> Result<(), ()> {
    client
        .post(format!("{base}/keypress/{key}"))
        .send()
        .await
        .map_err(|_| ())?
        .status()
        .is_success()
        .then_some(())
        .ok_or(())
}

/// One-shot SSDP M-SEARCH for Roku devices; returns (ip, name, model, serial).
pub async fn ssdp_discover() -> Vec<(String, String, String, String)> {
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
        if let Ok(resp) = client.get(format!("http://{ip}:8060/query/device-info")).send().await {
            if let Ok(xml) = resp.text().await {
                let tag = |t: &str| {
                    xml.split(&format!("<{t}>")).nth(1)
                        .and_then(|r| r.split(&format!("</{t}>")).next())
                        .unwrap_or("").to_string()
                };
                let name = {
                    let n = tag("user-device-name");
                    if n.is_empty() { tag("friendly-device-name") } else { n }
                };
                let model = tag("model-name");
                let serial = tag("serial-number");
                info!(ip=%ip, name=%name, model=%model, serial=%serial, "ssdp: found roku");
                found.push((ip.clone(), name, model, serial));
            }
        }
    }
    found
}
