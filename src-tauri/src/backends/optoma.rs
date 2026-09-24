//! Optoma projector backend: RS-232 command set over Telnet (port 23).
//! Experimental — verified against the user's UHZ60 at build time.
//! Volume range on most Optoma models is 0..10; we map 0..100%.
//! Commands are write-mostly; state queries are unreliable across models,
//! so we track assumed state locally.

use crate::types::{CoreEvent, DeviceCmd, TvStatus};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub struct OptomaActor {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub port: u16, // usually 23
    pub cmd_rx: mpsc::Receiver<DeviceCmd>,
    pub events: mpsc::Sender<CoreEvent>,
}

const CMD_GAP: Duration = Duration::from_millis(250); // spec: >200ms between commands

impl OptomaActor {
    pub async fn run(mut self) {
        let mut assumed_volume: f32 = 0.5;
        let mut assumed_muted = false;
        let mut poll = tokio::time::interval(Duration::from_secs(20));
        let mut online = false;

        loop {
            tokio::select! {
                _ = poll.tick() => {
                    let reachable = tokio::time::timeout(
                        Duration::from_secs(3),
                        TcpStream::connect((self.ip.as_str(), self.port)),
                    ).await.map(|r| r.is_ok()).unwrap_or(false);
                    if reachable != online {
                        online = reachable;
                        info!(id=%self.id, name=%self.name, online, "optoma: reachability changed");
                        let _ = self.events.send(CoreEvent::Online { id: self.id.clone(), online }).await;
                        if online {
                            // Power can be switched, but this write-only connection can't read the state back.
                            let tv = TvStatus { has_power: true, ..Default::default() };
                            let _ = self.events.send(CoreEvent::DeviceStatus { id: self.id.clone(), tv, macs: Vec::new() }).await;
                        }
                    }
                }
                cmd = self.cmd_rx.recv() => {
                    let cmd = match cmd { Some(c) => c, None => return };
                    if matches!(cmd, DeviceCmd::Shutdown) { return; }
                    info!(id=%self.id, name=%self.name, ?cmd, "optoma: sending command");
                    let raw = match &cmd {
                        DeviceCmd::SetVolume(level) => {
                            assumed_volume = *level;
                            let v = (level * 10.0).round() as i32;
                            Some(format!("~0081 {v}\r"))
                        }
                        DeviceCmd::SetMuted(m) => {
                            assumed_muted = *m;
                            Some(format!("~0080 {}\r", if *m { 1 } else { 0 })) // AV mute / audio mute
                        }
                        DeviceCmd::Refresh => None,
                        // Optoma RS-232 set: ~XX00 1 = power on, ~XX00 0 = power off (XX = projector ID 00).
                        DeviceCmd::Power(on) => Some(format!("~0000 {}\r", if *on { 1 } else { 0 })),
                        _ => None, // no transport controls
                    };
                    if let Some(raw) = raw {
                        match self.send_raw(&raw).await {
                            Ok(()) => {
                                let _ = self.events.send(CoreEvent::VolumeChanged {
                                    id: self.id.clone(), volume: assumed_volume, muted: assumed_muted,
                                }).await;
                            }
                            Err(e) => warn!(id=%self.id, error=%e, "optoma: send failed"),
                        }
                    }
                }
            }
        }
    }

    async fn send_raw(&self, raw: &str) -> Result<(), String> {
        let mut stream = tokio::time::timeout(
            Duration::from_secs(3),
            TcpStream::connect((self.ip.as_str(), self.port)),
        )
        .await
        .map_err(|_| "timeout".to_string())?
        .map_err(|e| e.to_string())?;
        stream.write_all(raw.as_bytes()).await.map_err(|e| e.to_string())?;
        tokio::time::sleep(CMD_GAP).await;
        Ok(())
    }
}
