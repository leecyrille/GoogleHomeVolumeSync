//! LG webOS TV backend via SSAP websocket (port 3000).
//! First connection triggers an on-screen pairing prompt; the returned
//! client-key is reported back to the core for persistence.

use crate::types::{CoreEvent, DeviceCmd};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

pub struct LgActor {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub client_key: Option<String>,
    pub cmd_rx: mpsc::Receiver<DeviceCmd>,
    pub events: mpsc::Sender<CoreEvent>,
    /// (device_id, client_key) reported after successful pairing.
    pub key_tx: mpsc::Sender<(String, String)>,
}

impl LgActor {
    pub async fn run(mut self) {
        let mut backoff = 2u64;
        loop {
            match self.session().await {
                SessionEnd::Shutdown => return,
                SessionEnd::Disconnected => {
                    let _ = self.events.send(CoreEvent::Online { id: self.id.clone(), online: false }).await;
                    debug!(id=%self.id, "lg: disconnected, retrying in {}s", backoff);
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(backoff);
                    loop {
                        match tokio::time::timeout_at(deadline, self.cmd_rx.recv()).await {
                            Ok(Some(DeviceCmd::Shutdown)) | Ok(None) => return,
                            Ok(Some(_)) => continue,
                            Err(_) => break,
                        }
                    }
                    backoff = (backoff * 2).min(120);
                }
            }
        }
    }

    async fn session(&mut self) -> SessionEnd {
        let url = format!("ws://{}:3000", self.ip);
        let (mut ws, _) = match tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::connect_async(&url)).await {
            Ok(Ok(ok)) => ok,
            _ => return SessionEnd::Disconnected,
        };
        info!(id=%self.id, name=%self.name, "lg: websocket connected, registering");

        let mut register = json!({
            "type": "register",
            "id": "register_0",
            "payload": {
                "manifest": {
                    "manifestVersion": 1,
                    "permissions": [
                        "LAUNCH", "CONTROL_AUDIO", "CONTROL_PLAYBACK",
                        "READ_CURRENT_CHANNEL", "READ_RUNNING_APPS", "CONTROL_POWER"
                    ]
                }
            }
        });
        if let Some(key) = &self.client_key {
            register["payload"]["client-key"] = json!(key);
        }
        if ws.send(Message::text(register.to_string())).await.is_err() {
            return SessionEnd::Disconnected;
        }

        let mut req_id = 100u64;
        let mut registered = false;

        loop {
            tokio::select! {
                msg = ws.next() => {
                    let Some(Ok(Message::Text(text))) = msg else {
                        if matches!(msg, Some(Ok(_))) { continue; }
                        return SessionEnd::Disconnected;
                    };
                    let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                    match v["type"].as_str().unwrap_or("") {
                        "registered" => {
                            registered = true;
                            if let Some(key) = v["payload"]["client-key"].as_str() {
                                if self.client_key.as_deref() != Some(key) {
                                    info!(id=%self.id, "lg: paired, storing client key");
                                    self.client_key = Some(key.to_string());
                                    let _ = self.key_tx.send((self.id.clone(), key.to_string())).await;
                                }
                            }
                            info!(id=%self.id, name=%self.name, "lg: registered");
                            let _ = self.events.send(CoreEvent::Online { id: self.id.clone(), online: true }).await;
                            req_id += 1;
                            let sub = json!({
                                "type": "subscribe", "id": format!("vol_{req_id}"),
                                "uri": "ssap://audio/getVolume"
                            });
                            let _ = ws.send(Message::text(sub.to_string())).await;
                        }
                        "response" => {
                            let p = &v["payload"];
                            // getVolume subscription responses (schema varies by webOS version)
                            let vol = p["volume"].as_i64()
                                .or_else(|| p["volumeStatus"]["volume"].as_i64());
                            if let Some(vol) = vol {
                                let muted = p["muted"].as_bool()
                                    .or_else(|| p["volumeStatus"]["muteStatus"].as_bool())
                                    .unwrap_or(false);
                                debug!(id=%self.id, vol, muted, "lg: volume status");
                                let _ = self.events.send(CoreEvent::VolumeChanged {
                                    id: self.id.clone(), volume: vol as f32 / 100.0, muted,
                                }).await;
                            }
                        }
                        "error" => {
                            warn!(id=%self.id, error=%v["error"].as_str().unwrap_or("?"), "lg: error message");
                            if !registered {
                                // Pairing rejected or timed out.
                                return SessionEnd::Disconnected;
                            }
                        }
                        _ => {}
                    }
                }
                cmd = self.cmd_rx.recv() => {
                    let cmd = match cmd { Some(c) => c, None => return SessionEnd::Shutdown };
                    if matches!(cmd, DeviceCmd::Shutdown) { return SessionEnd::Shutdown; }
                    if !registered { continue; }
                    info!(id=%self.id, name=%self.name, ?cmd, "lg: sending command");
                    req_id += 1;
                    let (uri, payload) = match cmd {
                        DeviceCmd::SetVolume(level) => ("ssap://audio/setVolume", json!({"volume": (level*100.0).round() as i64})),
                        DeviceCmd::SetMuted(m) => ("ssap://audio/setMute", json!({"mute": m})),
                        DeviceCmd::Play => ("ssap://media.controls/play", json!({})),
                        DeviceCmd::Pause => ("ssap://media.controls/pause", json!({})),
                        DeviceCmd::Next => ("ssap://media.controls/fastForward", json!({})),
                        DeviceCmd::Prev => ("ssap://media.controls/rewind", json!({})),
                        DeviceCmd::Refresh => ("ssap://audio/getVolume", json!({})),
                        DeviceCmd::Shutdown => unreachable!(),
                    };
                    let msg = json!({"type":"request","id":format!("req_{req_id}"),"uri":uri,"payload":payload});
                    if ws.send(Message::text(msg.to_string())).await.is_err() {
                        return SessionEnd::Disconnected;
                    }
                }
            }
        }
    }
}

enum SessionEnd {
    Shutdown,
    Disconnected,
}
