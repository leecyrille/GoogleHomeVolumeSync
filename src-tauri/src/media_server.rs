//! Tiny HTTP server that lets a TV stream a file the user picked on this PC.
//!
//! Only files explicitly shared are served, each under a random token
//! (`/v/<token>/<name>`), only to the device it was shared with, and only for
//! SHARE_LIFETIME. Range requests are supported so the TV can seek.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

const SHARE_LIFETIME: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);

struct Shared {
    path: PathBuf,
    /// The only address allowed to fetch this file (the TV it was shared with).
    allowed: std::net::IpAddr,
    expires: std::time::Instant,
}

struct Server {
    port: u16,
    files: Mutex<HashMap<String, Shared>>,
}

static SERVER: OnceLock<Server> = OnceLock::new();
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn server() -> Result<&'static Server, String> {
    if let Some(s) = SERVER.get() {
        return Ok(s);
    }
    let _guard = STARTING.lock().await;
    if let Some(s) = SERVER.get() {
        return Ok(s);
    }
    let listener = TcpListener::bind(("0.0.0.0", 0)).await.map_err(|e| format!("couldn't start the file server: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let _ = SERVER.set(Server { port, files: Mutex::new(HashMap::new()) });
    info!(port, "media server: listening");
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((sock, peer)) => {
                    tokio::spawn(async move {
                        if let Err(e) = handle(sock, peer.ip()).await {
                            debug!(%peer, error=%e, "media server: request ended");
                        }
                    });
                }
                Err(e) => warn!(error=%e, "media server: accept failed"),
            }
        }
    });
    Ok(SERVER.get().unwrap())
}

/// Share `path` and return the URL a device at `device_ip` can fetch it from.
pub async fn share(path: &Path, device_ip: &str) -> Result<String, String> {
    if !path.is_file() {
        return Err("That file doesn't exist.".into());
    }
    let allowed: std::net::IpAddr = device_ip.parse().map_err(|_| "The TV's address looks wrong.".to_string())?;
    let s = server().await?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    {
        let mut files = s.files.lock().unwrap();
        let now = std::time::Instant::now();
        files.retain(|_, f| f.expires > now);
        files.insert(token.clone(), Shared { path: path.to_path_buf(), allowed, expires: now + SHARE_LIFETIME });
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("video");
    let ip = local_ip_towards(device_ip).await.ok_or("Couldn't work out this PC's network address.")?;
    let url = format!("http://{ip}:{}/v/{token}/{}", s.port, encode(name));
    info!(file=%path.display(), %url, "media server: shared");
    Ok(url)
}

/// The local address the OS would use to reach `ip` (the right interface on multi-NIC PCs).
async fn local_ip_towards(ip: &str) -> Option<std::net::IpAddr> {
    let sock = tokio::net::UdpSocket::bind(("0.0.0.0", 0)).await.ok()?;
    sock.connect((ip, 8060)).await.ok()?;
    sock.local_addr().ok().map(|a| a.ip())
}

fn encode(s: &str) -> String {
    s.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

pub fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("mp4" | "m4v") => "video/mp4",
        Some("mkv") => "video/x-matroska",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("ts") => "video/mp2t",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("aac") => "audio/aac",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/wav",
        Some("ogg" | "opus") => "audio/ogg",
        Some("m3u8") => "application/x-mpegURL",
        Some("srt") => "application/x-subrip",
        Some("vtt") => "text/vtt",
        _ => "application/octet-stream",
    }
}

async fn handle(mut sock: TcpStream, peer: std::net::IpAddr) -> std::io::Result<()> {
    let mut buf = vec![0u8; 8192];
    let mut len = 0;
    // Read the request head.
    loop {
        let n = sock.read(&mut buf[len..]).await?;
        if n == 0 {
            return Ok(());
        }
        len += n;
        if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") || len == buf.len() {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf[..len]).to_string();
    let mut lines = head.lines();
    let mut req = lines.next().unwrap_or("").split_whitespace();
    let method = req.next().unwrap_or("");
    let target = req.next().unwrap_or("");
    let range = lines
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("range")).map(|(_, v)| v.trim().to_string()));

    let token = target.strip_prefix("/v/").and_then(|r| r.split('/').next()).unwrap_or("");
    let path = SERVER.get().and_then(|s| {
        s.files.lock().unwrap().get(token)
            .filter(|f| f.allowed == peer && f.expires > std::time::Instant::now())
            .map(|f| f.path.clone())
    });
    if path.is_none() {
        debug!(%peer, "media server: refused (unknown/expired link or wrong device)");
    }
    let Some(path) = path.filter(|_| method == "GET" || method == "HEAD") else {
        sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
        return Ok(());
    };

    let mut file = tokio::fs::File::open(&path).await?;
    let size = file.metadata().await?.len();
    // Only single ranges are needed by media players: bytes=start-[end]
    let (start, end, partial) = match range.as_deref().and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.split_once('-')) {
        Some((a, b)) => {
            let (start, end) = if a.is_empty() {
                // suffix range: last N bytes
                let n: u64 = b.parse().unwrap_or(0).min(size);
                (size - n, size.saturating_sub(1))
            } else {
                let start: u64 = a.parse().unwrap_or(0);
                let end: u64 = b.parse().ok().map(|e: u64| e.min(size.saturating_sub(1))).unwrap_or(size.saturating_sub(1));
                (start, end)
            };
            if start >= size || start > end {
                let msg = format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                sock.write_all(msg.as_bytes()).await?;
                return Ok(());
            }
            (start, end, true)
        }
        None => (0, size.saturating_sub(1), false),
    };
    let body_len = if size == 0 { 0 } else { end - start + 1 };
    let mut header = if partial {
        format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{size}\r\n")
    } else {
        "HTTP/1.1 200 OK\r\n".to_string()
    };
    header += &format!(
        "Content-Type: {}\r\nContent-Length: {body_len}\r\nAccept-Ranges: bytes\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        content_type(&path)
    );
    sock.write_all(header.as_bytes()).await?;
    if method == "HEAD" || body_len == 0 {
        return Ok(());
    }
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut limited = file.take(body_len);
    tokio::io::copy(&mut limited, &mut sock).await?;
    Ok(())
}

/// Cast devices only show WebVTT subtitles: convert an .srt next to the video
/// into a temporary .vtt file (a .vtt is returned as-is).
pub fn subtitles_as_vtt(sub: &Path) -> Option<PathBuf> {
    let is_vtt = sub.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("vtt")).unwrap_or(false);
    if is_vtt {
        return Some(sub.to_path_buf());
    }
    let text = std::fs::read_to_string(sub).ok()?;
    let text = text.trim_start_matches('\u{feff}');
    let mut out = String::from("WEBVTT\n\n");
    for line in text.lines() {
        // 00:00:01,234 --> 00:00:03,000  becomes  00:00:01.234 --> 00:00:03.000
        if line.contains("-->") {
            out.push_str(&line.replace(',', "."));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    let dir = std::env::temp_dir().join("volume-sync-subtitles");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.vtt", uuid::Uuid::new_v4().simple()));
    std::fs::write(&path, out).ok()?;
    Some(path)
}
