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
    /// Roku developer-installer passwords: device id -> password (local only).
    #[serde(default)]
    pub roku_dev_passwords: HashMap<String, String>,
    /// LG webOS pairing keys: device id -> client-key.
    #[serde(default)]
    pub lg_keys: HashMap<String, String>,
    /// Showing the PactoTech Calendar Saver on TVs.
    #[serde(default)]
    pub calendar: CalendarConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalendarConfig {
    /// Secret part of the pictures' address, kept so screensavers keep working.
    #[serde(default)]
    pub token: String,
    /// Roku TVs using the calendar as their screensaver.
    #[serde(default)]
    pub screensaver_tvs: Vec<String>,
    /// The settings each of those TVs saved: device id -> launch parameters.
    #[serde(default)]
    pub pushed: HashMap<String, String>,
    /// "dark" or "light", unless a schedule says otherwise.
    #[serde(default = "default_dark")]
    pub theme: String,
    /// Views when shown by hand: "month", "week", "day".
    #[serde(default = "default_views")]
    pub views: Vec<String>,
    /// Seconds between views (0 = stay on the first).
    #[serde(default)]
    pub rotate_secs: u32,
    /// Sharp 4K on Roku TVs (sent as a one-frame video).
    #[serde(default)]
    pub four_k: bool,
    #[serde(default)]
    pub feeds: Vec<crate::cal_feeds::FeedCfg>,
    #[serde(default)]
    pub photo_folders: Vec<String>,
    #[serde(default = "default_photo_secs")]
    pub photo_interval_secs: u32,
    #[serde(default = "default_refresh")]
    pub refresh_minutes: u32,
    #[serde(default)]
    pub schedules: Vec<CalSchedule>,
    /// The PactoTech Calendar Saver's settings were copied once already.
    #[serde(default)]
    pub saver_imported: bool,
}
fn default_dark() -> String {
    "dark".into()
}
fn default_views() -> Vec<String> {
    vec!["month".into()]
}
fn default_photo_secs() -> u32 {
    20
}
fn default_refresh() -> u32 {
    15
}
impl Default for CalendarConfig {
    fn default() -> Self {
        CalendarConfig {
            token: String::new(), screensaver_tvs: Vec::new(), pushed: HashMap::new(), theme: default_dark(),
            views: default_views(), rotate_secs: 0, four_k: false, feeds: Vec::new(), photo_folders: Vec::new(),
            photo_interval_secs: default_photo_secs(), refresh_minutes: default_refresh(), schedules: Vec::new(), saver_imported: false,
        }
    }
}

/// Show the calendar on some screens at a set time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalSchedule {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Mon..Sun
    pub days: [bool; 7],
    /// "HH:MM" local
    pub start: String,
    pub duration_min: u32,
    #[serde(default)]
    pub devices: Vec<String>,
    /// "light", "dark" or "default"
    #[serde(default)]
    pub theme: String,
    #[serde(default = "default_views")]
    pub views: Vec<String>,
    #[serde(default)]
    pub rotate_secs: u32,
    /// Turn a TV on if it's off.
    #[serde(default = "default_true")]
    pub power_on: bool,
    /// Leave a TV alone while it's playing something (screensavers and the home screen are fine).
    #[serde(default = "default_true")]
    pub dont_interrupt: bool,
    /// When the time is up, turn the TV off (if it's still showing the calendar).
    #[serde(default = "default_true")]
    pub off_after: bool,
    /// Turn it off early after this many minutes without a button press (0 = never).
    #[serde(default)]
    pub idle_off_min: u32,
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
