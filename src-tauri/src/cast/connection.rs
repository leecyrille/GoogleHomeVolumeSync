//! Per-device Cast v2 connection actor: TLS to port 8009, heartbeat,
//! receiver (volume) + media (transport) channels, auto-reconnect.

use super::proto::*;
use crate::types::{CastItem, CoreEvent, DeviceCmd, MediaInfo};
use prost::Message;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::mpsc;
use tokio_native_tls::TlsStream;
use tracing::{debug, info, warn};

type Stream = TlsStream<tokio::net::TcpStream>;

static REQ_ID: AtomicI32 = AtomicI32::new(1000);
fn next_req_id() -> i32 {
    REQ_ID.fetch_add(1, Ordering::Relaxed)
}

pub struct CastActor {
    pub id: String,
    pub name: String, // for logging
    pub ip: String,
    pub port: u16,
    pub is_group: bool,
    pub cmd_rx: mpsc::Receiver<DeviceCmd>,
    pub events: mpsc::Sender<CoreEvent>,
}

struct SessionState {
    media_transport_id: Option<String>,
    media_session_id: Option<i64>,
    app_name: Option<String>,
    track: Track,
    /// Items waiting for the Default Media Receiver to start.
    pending_cast: Option<Vec<CastItem>>,
    /// Pictures being shown as a slideshow, the current index, and when to advance.
    slideshow: Option<(Vec<CastItem>, usize)>,
    next_slide: tokio::time::Instant,
}

/// How long each picture stays up in a slideshow.
const SLIDE_INTERVAL: Duration = Duration::from_secs(8);

/// Google's Default Media Receiver: plays a URL on any Cast device.
const DEFAULT_MEDIA_RECEIVER: &str = "CC1AD845";

#[derive(Default)]
struct Track {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    image: Option<String>,
    duration_ms: Option<u64>,
}

impl CastActor {
    pub async fn run(mut self) {
        let mut backoff = 1u64;
        loop {
            match self.connect().await {
                Ok(stream) => {
                    backoff = 1;
                    info!(id=%self.id, name=%self.name, ip=%self.ip, port=self.port, "cast: connected");
                    let _ = self.events.send(CoreEvent::Online { id: self.id.clone(), online: true }).await;
                    let shutdown = self.session(stream).await;
                    let _ = self.events.send(CoreEvent::Online { id: self.id.clone(), online: false }).await;
                    if shutdown {
                        info!(id=%self.id, "cast: actor shut down");
                        return;
                    }
                    info!(id=%self.id, name=%self.name, "cast: disconnected, will reconnect");
                }
                Err(e) => {
                    debug!(id=%self.id, ip=%self.ip, error=%e, "cast: connect failed");
                }
            }
            // Drain commands while offline so senders don't back up; honor Shutdown.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(backoff);
            loop {
                match tokio::time::timeout_at(deadline, self.cmd_rx.recv()).await {
                    Ok(Some(DeviceCmd::Shutdown)) | Ok(None) => return,
                    Ok(Some(_)) => continue,
                    Err(_) => break,
                }
            }
            backoff = (backoff * 2).min(60);
        }
    }

    async fn connect(&self) -> Result<Stream, String> {
        let tcp = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::TcpStream::connect((self.ip.as_str(), self.port)),
        )
        .await
        .map_err(|_| "timeout".to_string())?
        .map_err(|e| e.to_string())?;
        let connector = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()
            .map_err(|e| e.to_string())?;
        let connector = tokio_native_tls::TlsConnector::from(connector);
        connector.connect(&self.ip, tcp).await.map_err(|e| e.to_string())
    }

    /// Returns true if the actor should shut down permanently.
    async fn session(&mut self, stream: Stream) -> bool {
        let (mut rd, mut wr) = tokio::io::split(stream);
        let mut state = SessionState { media_transport_id: None, media_session_id: None, app_name: None, track: Track::default(), pending_cast: None,
            slideshow: None, next_slide: tokio::time::Instant::now() };

        if send(&mut wr, &self.id, "receiver-0", NS_CONNECTION, &json!({"type":"CONNECT"})).await.is_err() {
            return false;
        }
        let _ = send(&mut wr, &self.id, "receiver-0", NS_RECEIVER, &json!({"type":"GET_STATUS","requestId":next_req_id()})).await;
        if self.is_group {
            let _ = send(&mut wr, &self.id, "receiver-0", NS_MULTIZONE, &json!({"type":"GET_STATUS","requestId":next_req_id()})).await;
        }

        let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
        let mut last_pong = tokio::time::Instant::now();

        loop {
            tokio::select! {
                msg = read_frame(&mut rd) => {
                    let msg = match msg {
                        Ok(m) => m,
                        Err(e) => {
                            warn!(id=%self.id, name=%self.name, error=%e, "cast: read error");
                            return false;
                        }
                    };
                    last_pong = tokio::time::Instant::now();
                    if let Some(payload) = msg.payload_utf8.as_deref() {
                        if let Ok(v) = serde_json::from_str::<Value>(payload) {
                            self.handle_message(&msg.source_id, &msg.namespace, &v, &mut wr, &mut state).await;
                        }
                    }
                }
                _ = tokio::time::sleep_until(state.next_slide), if state.slideshow.is_some() => {
                    state.next_slide = tokio::time::Instant::now() + SLIDE_INTERVAL;
                    if let (Some((items, idx)), Some(tid)) = (state.slideshow.as_mut(), state.media_transport_id.clone()) {
                        *idx = (*idx + 1) % items.len();
                        let payload = load_one(&items[*idx]);
                        let _ = send(&mut wr, &self.id, &tid, NS_MEDIA, &payload).await;
                    }
                }
                _ = heartbeat.tick() => {
                    if last_pong.elapsed() > Duration::from_secs(30) {
                        warn!(id=%self.id, "cast: heartbeat timeout");
                        return false;
                    }
                    if send(&mut wr, &self.id, "receiver-0", NS_HEARTBEAT, &json!({"type":"PING"})).await.is_err() {
                        return false;
                    }
                }
                cmd = self.cmd_rx.recv() => {
                    let cmd = match cmd { Some(c) => c, None => return true };
                    if matches!(cmd, DeviceCmd::Shutdown) { return true; }
                    if self.handle_cmd(cmd, &mut wr, &mut state).await.is_err() { return false; }
                }
            }
        }
    }

    async fn handle_cmd(
        &self,
        cmd: DeviceCmd,
        wr: &mut WriteHalf<Stream>,
        state: &mut SessionState,
    ) -> std::io::Result<()> {
        if !matches!(cmd, DeviceCmd::PollMedia) {
            info!(id=%self.id, name=%self.name, ?cmd, "cast: sending command");
        }
        match cmd {
            DeviceCmd::SetVolume(level) => {
                send(wr, &self.id, "receiver-0", NS_RECEIVER,
                    &json!({"type":"SET_VOLUME","requestId":next_req_id(),"volume":{"level":level}})).await
            }
            DeviceCmd::SetMuted(m) => {
                send(wr, &self.id, "receiver-0", NS_RECEIVER,
                    &json!({"type":"SET_VOLUME","requestId":next_req_id(),"volume":{"muted":m}})).await
            }
            DeviceCmd::Refresh => {
                send(wr, &self.id, "receiver-0", NS_RECEIVER,
                    &json!({"type":"GET_STATUS","requestId":next_req_id()})).await
            }
            DeviceCmd::Play | DeviceCmd::Pause | DeviceCmd::Next | DeviceCmd::Prev => {
                let (Some(tid), Some(msid)) = (state.media_transport_id.as_ref(), state.media_session_id) else {
                    debug!(id=%self.id, "cast: no media session for transport command");
                    return Ok(());
                };
                let payload = match cmd {
                    DeviceCmd::Play => json!({"type":"PLAY","requestId":next_req_id(),"mediaSessionId":msid}),
                    DeviceCmd::Pause => json!({"type":"PAUSE","requestId":next_req_id(),"mediaSessionId":msid}),
                    DeviceCmd::Next => json!({"type":"QUEUE_UPDATE","requestId":next_req_id(),"mediaSessionId":msid,"jump":1}),
                    _ => json!({"type":"QUEUE_UPDATE","requestId":next_req_id(),"mediaSessionId":msid,"jump":-1}),
                };
                send(wr, &self.id, tid, NS_MEDIA, &payload).await
            }
            DeviceCmd::Seek(ms) => {
                let (Some(tid), Some(msid)) = (state.media_transport_id.as_ref(), state.media_session_id) else {
                    return Ok(());
                };
                send(wr, &self.id, tid, NS_MEDIA,
                    &json!({"type":"SEEK","requestId":next_req_id(),"mediaSessionId":msid,"currentTime": ms as f64 / 1000.0})).await
            }
            DeviceCmd::Cast(items) => {
                // Start the Default Media Receiver; the queue is loaded once it reports running.
                // Several pictures become a slideshow the app advances itself.
                state.slideshow = None;
                if items.len() > 1 && items.iter().all(|i| i.is_image()) {
                    state.slideshow = Some((items.clone(), 0));
                    state.next_slide = tokio::time::Instant::now() + SLIDE_INTERVAL;
                    state.pending_cast = Some(vec![items[0].clone()]);
                } else {
                    state.pending_cast = Some(items);
                }
                send(wr, &self.id, "receiver-0", NS_RECEIVER,
                    &json!({"type":"LAUNCH","requestId":next_req_id(),"appId":DEFAULT_MEDIA_RECEIVER})).await
            }
            DeviceCmd::PollMedia => match state.media_transport_id.as_ref() {
                Some(tid) => send(wr, &self.id, tid, NS_MEDIA, &json!({"type":"GET_STATUS","requestId":next_req_id()})).await,
                None => Ok(()),
            },
            DeviceCmd::StopCasting => {
                state.slideshow = None;
                state.pending_cast = None;
                match (state.media_transport_id.as_ref(), state.media_session_id) {
                    (Some(tid), Some(msid)) => send(wr, &self.id, tid, NS_MEDIA,
                        &json!({"type":"STOP","requestId":next_req_id(),"mediaSessionId":msid})).await,
                    _ => Ok(()),
                }
            }
            DeviceCmd::Power(_) | DeviceCmd::Input(_) | DeviceCmd::Key(_) | DeviceCmd::Resync | DeviceCmd::Shutdown => Ok(()),
        }
    }

    async fn handle_message(
        &self,
        source: &str,
        namespace: &str,
        v: &Value,
        wr: &mut WriteHalf<Stream>,
        state: &mut SessionState,
    ) {
        let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match (namespace, msg_type) {
            (NS_HEARTBEAT, "PING") => {
                let _ = send(wr, &self.id, source, NS_HEARTBEAT, &json!({"type":"PONG"})).await;
            }
            (NS_HEARTBEAT, "PONG") => {}
            (NS_RECEIVER, "RECEIVER_STATUS") => {
                let status = &v["status"];
                if let Some(level) = status["volume"]["level"].as_f64() {
                    let muted = status["volume"]["muted"].as_bool().unwrap_or(false);
                    debug!(id=%self.id, name=%self.name, volume=level, muted, "cast: receiver status");
                    let _ = self.events.send(CoreEvent::VolumeChanged {
                        id: self.id.clone(), volume: level as f32, muted,
                    }).await;
                }
                // Find a running app that supports the media namespace.
                let media_app = status["applications"].as_array().and_then(|apps| {
                    apps.iter().find(|a| {
                        a["namespaces"].as_array().map(|ns| {
                            ns.iter().any(|n| n["name"].as_str() == Some(NS_MEDIA))
                        }).unwrap_or(false)
                            && !a["isIdleScreen"].as_bool().unwrap_or(false)
                    })
                });
                match media_app {
                    Some(app) => {
                        let tid = app["transportId"].as_str().unwrap_or("").to_string();
                        let app_name = app["displayName"].as_str().unwrap_or("").to_string();
                        state.app_name = Some(app_name.trim_end_matches(" (Remote Control)").to_string())
                            .filter(|n| !n.is_empty());
                        if state.media_transport_id.as_deref() != Some(tid.as_str()) && !tid.is_empty() {
                            info!(id=%self.id, name=%self.name, app=%app_name, transport=%tid, "cast: media app running, connecting");
                            state.media_transport_id = Some(tid.clone());
                            state.media_session_id = None;
                            state.track = Track::default();
                            let _ = send(wr, &self.id, &tid, NS_CONNECTION, &json!({"type":"CONNECT"})).await;
                            let _ = send(wr, &self.id, &tid, NS_MEDIA, &json!({"type":"GET_STATUS","requestId":next_req_id()})).await;
                        }
                        if app["appId"].as_str() == Some(DEFAULT_MEDIA_RECEIVER) && !tid.is_empty() {
                            if let Some(items) = state.pending_cast.take() {
                                info!(id=%self.id, name=%self.name, count = items.len(), "cast: loading queue");
                                let _ = send(wr, &self.id, &tid, NS_MEDIA, &queue_load(&items)).await;
                            }
                        }
                    }
                    None => {
                        state.app_name = None;
                        state.slideshow = None;
                        if state.media_transport_id.take().is_some() {
                            state.media_session_id = None;
                            state.track = Track::default();
                            let _ = self.events.send(CoreEvent::MediaChanged { id: self.id.clone(), media: None }).await;
                        }
                    }
                }
            }
            (NS_MEDIA, "MEDIA_STATUS") => {
                let empty = vec![];
                let statuses = v["status"].as_array().unwrap_or(&empty);
                if let Some(s) = statuses.first() {
                    let session = s["mediaSessionId"].as_i64();
                    if session != state.media_session_id {
                        state.track = Track::default();
                    }
                    state.media_session_id = session;
                    // Most status updates omit "media"; only the ones sent on a
                    // track change carry metadata, so keep the last known track.
                    if let Some(d) = s["media"]["duration"].as_f64().filter(|d| *d > 0.0) {
                        state.track.duration_ms = Some((d * 1000.0) as u64);
                    }
                    let meta = &s["media"]["metadata"];
                    if meta.is_object() {
                        let text = |k: &str| meta[k].as_str().filter(|t| !t.is_empty()).map(String::from);
                        state.track = Track {
                            title: text("title"),
                            artist: text("artist").or_else(|| text("albumArtist")).or_else(|| text("subtitle")),
                            album: text("albumName"),
                            image: meta["images"].as_array()
                                .and_then(|imgs| imgs.first())
                                .and_then(|i| i["url"].as_str())
                                .map(String::from),
                            duration_ms: state.track.duration_ms,
                        };
                    }
                    let position_ms = s["currentTime"].as_f64().map(|t| (t * 1000.0) as u64);
                    let media = MediaInfo {
                        state: s["playerState"].as_str().unwrap_or("IDLE").to_string(),
                        title: state.track.title.clone(),
                        artist: state.track.artist.clone(),
                        app: state.app_name.clone(),
                        supports_transport: true,
                        album: state.track.album.clone(),
                        image: state.track.image.clone(),
                        position_ms,
                        duration_ms: state.track.duration_ms,
                        position_at: position_ms.map(|_| unix_ms()),
                    };
                    debug!(id=%self.id, name=%self.name, state=%media.state, title=?media.title, "cast: media status");
                    let _ = self.events.send(CoreEvent::MediaChanged { id: self.id.clone(), media: Some(media) }).await;
                } else if statuses.is_empty() && state.media_session_id.is_some() {
                    // keep session; some apps send empty status on idle transitions
                }
            }
            (NS_MULTIZONE, "MULTIZONE_STATUS") => {
                let members: Vec<String> = v["status"]["devices"].as_array()
                    .map(|ds| ds.iter()
                        .filter_map(|d| d["deviceId"].as_str())
                        .map(|s| s.to_lowercase().replace('-', ""))
                        .collect())
                    .unwrap_or_default();
                info!(id=%self.id, name=%self.name, count=members.len(), "cast: multizone group members");
                let _ = self.events.send(CoreEvent::GroupMembers { id: self.id.clone(), members }).await;
            }
            (NS_MULTIZONE, "DEVICE_ADDED") | (NS_MULTIZONE, "DEVICE_UPDATED") | (NS_MULTIZONE, "DEVICE_REMOVED") => {
                let _ = send(wr, &self.id, "receiver-0", NS_MULTIZONE, &json!({"type":"GET_STATUS","requestId":next_req_id()})).await;
            }
            (NS_CONNECTION, "CLOSE") => {
                if Some(source) == state.media_transport_id.as_deref() {
                    state.media_transport_id = None;
                    state.media_session_id = None;
                    let _ = self.events.send(CoreEvent::MediaChanged { id: self.id.clone(), media: None }).await;
                }
            }
            _ => {}
        }
    }
}

async fn send(
    wr: &mut WriteHalf<Stream>,
    log_id: &str,
    dest: &str,
    namespace: &str,
    payload: &Value,
) -> std::io::Result<()> {
    let msg = text_msg(dest, namespace, payload);
    if namespace != NS_HEARTBEAT {
        debug!(id=%log_id, dest=%dest, ns=%namespace, payload=%payload, "cast: tx");
    }
    let mut buf = Vec::with_capacity(msg.encoded_len() + 4);
    buf.extend_from_slice(&(msg.encoded_len() as u32).to_be_bytes());
    msg.encode(&mut buf).expect("encode");
    wr.write_all(&buf).await
}

async fn read_frame(rd: &mut ReadHalf<Stream>) -> std::io::Result<CastMessage> {
    let mut len_buf = [0u8; 4];
    rd.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 1024 * 1024 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut buf = vec![0u8; len];
    rd.read_exact(&mut buf).await?;
    CastMessage::decode(buf.as_slice())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// QUEUE_LOAD for the Default Media Receiver, with WebVTT subtitles when given.
fn queue_load(items: &[CastItem]) -> Value {
    let items: Vec<Value> = items.iter().map(|it| {
        let mut media = media_json(it);
        let mut item = json!({ "media": media.clone(), "autoplay": it.autoplay, "preloadTime": 10 });
        if let Some(sub) = &it.subtitles {
            media["tracks"] = json!([{
                "trackId": 1, "type": "TEXT", "trackContentId": sub, "trackContentType": "text/vtt",
                "subtype": "SUBTITLES", "name": "Subtitles", "language": "en-US"
            }]);
            item = json!({ "media": media, "autoplay": it.autoplay, "preloadTime": 10, "activeTrackIds": [1] });
        }
        item
    }).collect();
    json!({ "type": "QUEUE_LOAD", "requestId": next_req_id(), "items": items, "startIndex": 0, "repeatMode": "REPEAT_OFF" })
}

fn media_json(it: &CastItem) -> Value {
    // metadataType: 0 generic, 3 music, 4 photo. Pictures have no stream.
    let (kind, stream) = if it.is_image() {
        (4, "NONE")
    } else if it.content_type.starts_with("audio/") {
        (3, "BUFFERED")
    } else {
        (0, "BUFFERED")
    };
    json!({
        "contentId": it.url,
        "contentUrl": it.url,
        "contentType": it.content_type,
        "streamType": stream,
        "metadata": { "metadataType": kind, "title": it.title },
    })
}

/// LOAD a single item (used to advance a picture slideshow).
fn load_one(it: &CastItem) -> Value {
    json!({ "type": "LOAD", "requestId": next_req_id(), "media": media_json(it), "autoplay": true })
}
