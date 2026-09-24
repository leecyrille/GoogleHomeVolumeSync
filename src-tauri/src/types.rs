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
    #[serde(default)]
    pub album: Option<String>,
    /// Artwork URL, when the source provides one.
    #[serde(default)]
    pub image: Option<String>,
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
    /// Screen state for TVs that report it (Roku).
    #[serde(default)]
    pub tv: Option<TvStatus>,
    /// For Google cast groups: the device ids of the speakers in it.
    #[serde(default)]
    pub members: Vec<String>,
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
    Power(bool),
    /// An `InputOption::id`.
    Input(String),
    /// A remote-control key, e.g. "Home" or "Up".
    Key(String),
    /// Move playback to this position (milliseconds).
    Seek(u64),
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct InputOption {
    pub id: String,
    pub label: String,
    /// "input" (HDMI, Live TV..) or "app" (Netflix..).
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct TvStatus {
    /// "Control by mobile apps" is Limited: power, inputs and app queries are refused.
    pub restricted: bool,
    /// Can be switched on and off from the app.
    #[serde(default)]
    pub has_power: bool,
    /// Current screen power; None when the device doesn't report it.
    pub power: Option<bool>,
    /// What's on screen: an input ("Nintendo Switch") or an app ("Netflix").
    pub showing: Option<String>,
    pub showing_icon: Option<String>,
    /// Extra context: the Live TV channel and programme, or "Playing" in an app.
    pub showing_detail: Option<String>,
    /// What the TV is doing: off, screensaver, home, playing, paused, loading,
    /// live-tv, input (an HDMI source; can't see inside) or app (open, no playback reported).
    #[serde(default)]
    pub activity: Option<String>,
    /// Playback position and length in the current app, when the app reports them.
    #[serde(default)]
    pub position_ms: Option<u64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// When position_ms was read (unix ms), so the UI can advance it between polls.
    #[serde(default)]
    pub position_at: Option<i64>,
    #[serde(default)]
    pub is_live: bool,
    pub inputs: Vec<InputOption>,
    /// Headphones plugged into the remote or app (private listening).
    pub headphones: bool,
    pub model: Option<String>,
    pub firmware: Option<String>,
}

/// Events emitted by backend actors toward the core.
#[derive(Clone, Debug)]
pub enum CoreEvent {
    VolumeChanged { id: String, volume: f32, muted: bool },
    MediaChanged { id: String, media: Option<MediaInfo> },
    Online { id: String, online: bool },
    /// Members of a Google cast group (normalized device ids), via multizone.
    GroupMembers { id: String, members: Vec<String> },
    /// Power/input state from devices that report it, plus MAC addresses for wake-on-LAN.
    DeviceStatus { id: String, tv: TvStatus, macs: Vec<String> },
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
