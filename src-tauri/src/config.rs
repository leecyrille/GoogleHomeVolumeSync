use crate::types::{AppGroup, Backend, DeviceInfo, ScheduleEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_true")]
    pub start_with_windows: bool,
    #[serde(default)]
    pub auto_update: bool,
}
fn default_true() -> bool {
    true
}
impl Default for Settings {
    fn default() -> Self {
        Settings { start_with_windows: true, auto_update: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManualDevice {
    pub backend: Backend,
    pub ip: String,
    pub port: u16,
    pub name: String,
    /// Learned from the device, for wake-on-LAN.
    #[serde(default)]
    pub macs: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub groups: Vec<AppGroup>,
    #[serde(default)]
    pub schedules: Vec<ScheduleEvent>,
    /// Persisted device snapshots (for custom names / last_seen across restarts).
    #[serde(default)]
    pub known_devices: Vec<DeviceInfo>,
    /// Manually added devices (Yamaha / LG / Optoma / Roku by IP).
    #[serde(default)]
    pub manual_devices: Vec<ManualDevice>,
    /// Roku pseudo-absolute volume cache: device id -> last known level 0-100.
    #[serde(default)]
    pub roku_levels: HashMap<String, u8>,
    /// LG webOS pairing keys: device id -> client-key.
    #[serde(default)]
    pub lg_keys: HashMap<String, String>,
}

pub fn config_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("GoogleHomeVolumeSync");
    if !dir.exists() {
        // One-time migration from the pre-rebrand directory name.
        let old = base.join("PactoCastSync");
        if old.exists() {
            let _ = std::fs::rename(&old, &dir);
        }
    }
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn load() -> AppConfig {
    match std::fs::read_to_string(config_path()) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                warn!(error=%e, "config: parse failed, using defaults");
                AppConfig::default()
            }
        },
        Err(_) => {
            info!("config: no config file yet, using defaults");
            AppConfig::default()
        }
    }
}

pub fn save(cfg: &AppConfig) {
    match serde_json::to_string_pretty(cfg) {
        Ok(json) => {
            if let Err(e) = std::fs::write(config_path(), json) {
                warn!(error=%e, "config: save failed");
            }
        }
        Err(e) => warn!(error=%e, "config: serialize failed"),
    }
}
