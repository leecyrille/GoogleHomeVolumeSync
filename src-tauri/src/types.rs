use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Cast,
    Roku,
    Yamaha,
    Lg,
    Optoma,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct MediaInfo {
    pub state: String, // PLAYING / PAUSED / IDLE / BUFFERING
    pub title: Option<String>,
    pub artist: Option<String>,
    pub app: Option<String>,
    pub supports_transport: bool,
}

/// Snapshot of a device sent to the frontend and persisted (metadata parts).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String, // cast UUID, or "<backend>:<ip/serial>" for others
    pub backend: Backend,
    pub friendly_name: String,
    pub custom_name: Option<String>,
    pub model: String,
    pub ip: String,
    pub port: u16,
    pub is_cast_group: bool,
    pub online: bool,
    pub last_seen: i64, // unix seconds
    pub volume: f32,    // 0.0..1.0
    pub muted: bool,
    pub can_absolute_volume: bool,
    /// Per-device balance multiplier (0.0..2.0, default 1.0). Actual volume =
    /// group/logical volume x gain; the device reports logical = actual / gain
    /// to the group for sync purposes.
    #[serde(default = "default_gain")]
    pub sync_gain: f32,
    #[serde(default)]
    pub media: Option<MediaInfo>,
}

/// Commands accepted by every backend actor.
#[derive(Clone, Debug)]
pub enum DeviceCmd {
    SetVolume(f32),
    SetMuted(bool),
    Play,
    Pause,
    Next,
    Prev,
    Refresh,
    Shutdown,
}

/// Events emitted by backend actors toward the core.
#[derive(Clone, Debug)]
pub enum CoreEvent {
    VolumeChanged { id: String, volume: f32, muted: bool },
    MediaChanged { id: String, media: Option<MediaInfo> },
    Online { id: String, online: bool },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppGroup {
    pub id: String,
    pub name: String,
    pub member_ids: Vec<String>,
    pub sync_enabled: bool,
    #[serde(default = "default_volume")]
    pub group_volume: f32,
}
fn default_volume() -> f32 {
    0.5
}
pub fn default_gain() -> f32 {
    1.0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduleTarget {
    pub target_id: String, // group id or device id
    pub is_group: bool,
    pub volume_pct: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduleEvent {
    pub id: String,
    pub enabled: bool,
    pub days: [bool; 7], // Mon..Sun
    pub time: String,    // "HH:MM" local
    pub targets: Vec<ScheduleTarget>,
}
