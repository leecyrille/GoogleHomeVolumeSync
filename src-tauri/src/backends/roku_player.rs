//! "Play a video" on Roku TVs.
//!
//! Roku removed the built-in way to push a video URL to a TV, so the app
//! installs its own tiny player channel (roku-player/ in the repo) through
//! Roku's developer installer, then launches it with the file's URL. The TV
//! must be in developer mode, which the user enables once with a remote
//! sequence and a password of their choosing.

use std::io::Write;
use std::time::Duration;
use tracing::{info, warn};

const MANIFEST: &str = include_str!("../../../roku-player/manifest");
const MAIN_BRS: &str = include_str!("../../../roku-player/source/main.brs");
const SCENE_XML: &str = include_str!("../../../roku-player/components/PlayerScene.xml");
const SCENE_BRS: &str = include_str!("../../../roku-player/components/PlayerScene.brs");
const CAL_XML: &str = include_str!("../../../roku-player/components/CalendarView.xml");
const CAL_BRS: &str = include_str!("../../../roku-player/components/CalendarView.brs");
const SAVER_XML: &str = include_str!("../../../roku-player/components/CalendarSaverScene.xml");
const ICON: &[u8] = include_bytes!("../../../roku-player/images/icon.png");

/// Home x3, Up x2, Right, Left, Right, Left, Right opens Roku's developer settings.
const DEV_SEQUENCE: &[&str] = &["Home", "Home", "Home", "Up", "Up", "Right", "Left", "Right", "Left", "Right"];

fn client() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap()
}

/// Press the remote sequence that opens the developer settings screen.
pub async fn open_dev_settings(ip: &str) -> Result<(), String> {
    let c = client();
    for key in DEV_SEQUENCE {
        let r = c.post(format!("http://{ip}:8060/keypress/{key}")).send().await.map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(format!("The TV refused the {key} key ({}).", r.status()));
        }
        tokio::time::sleep(Duration::from_millis(if *key == "Home" { 700 } else { 350 })).await;
    }
    info!(ip, "roku player: sent developer-settings sequence");
    Ok(())
}

fn build_zip() -> Result<Vec<u8>, String> {
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut out);
        let opts: zip::write::SimpleFileOptions =
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in [
            ("manifest", MANIFEST.as_bytes()),
            ("source/main.brs", MAIN_BRS.as_bytes()),
            ("components/PlayerScene.xml", SCENE_XML.as_bytes()),
            ("components/PlayerScene.brs", SCENE_BRS.as_bytes()),
            ("components/CalendarView.xml", CAL_XML.as_bytes()),
            ("components/CalendarView.brs", CAL_BRS.as_bytes()),
            ("components/CalendarSaverScene.xml", SAVER_XML.as_bytes()),
            ("images/icon.png", ICON),
        ] {
            zip.start_file(name, opts).map_err(|e| e.to_string())?;
            zip.write_all(data).map_err(|e| e.to_string())?;
        }
        zip.finish().map_err(|e| e.to_string())?;
    }
    Ok(out.into_inner())
}

/// Install (or replace) the player channel via the developer installer on port 80.
/// The installer uses HTTP digest auth with user "rokudev".
pub async fn install(ip: &str, password: &str) -> Result<(), String> {
    let zip = build_zip()?;
    let url = format!("http://{ip}/plugin_install");
    let c = client();
    let form = || {
        reqwest::multipart::Form::new()
            .text("mysubmit", "Replace")
            .part("archive", reqwest::multipart::Part::bytes(zip.clone()).file_name("volume-sync-player.zip").mime_str("application/zip").unwrap())
    };

    let first = c.post(&url).multipart(form()).send().await
        .map_err(|_| "The TV's developer installer isn't answering. Is developer mode enabled and has the TV restarted?".to_string())?;
    let resp = if first.status() == reqwest::StatusCode::UNAUTHORIZED {
        let challenge = first.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let auth = digest_header(&challenge, "rokudev", password, "POST", "/plugin_install")
            .ok_or("The TV asked for an unexpected kind of login.")?;
        c.post(&url).header("Authorization", auth).multipart(form()).send().await.map_err(|e| e.to_string())?
    } else {
        first
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err("Wrong developer password for this TV.".into());
    }
    if !status.is_success() {
        warn!(ip, %status, "roku player: install failed");
        return Err(format!("The TV rejected the install ({status})."));
    }
    if body.contains("Install Success") || body.contains("Identical to previous version") || body.contains("Received") {
        info!(ip, "roku player: installed");
        Ok(())
    } else {
        warn!(ip, body=%body.chars().take(300).collect::<String>(), "roku player: unexpected installer response");
        Err("The TV didn't confirm the install. Try again, or check the password.".into())
    }
}

fn md5_hex(s: &str) -> String {
    format!("{:x}", md5::compute(s.as_bytes()))
}

/// RFC 2617 digest response for the Roku installer's challenge.
fn digest_header(challenge: &str, user: &str, pass: &str, method: &str, uri: &str) -> Option<String> {
    let rest = challenge.trim().strip_prefix("Digest")?;
    let field = |name: &str| {
        rest.split(',').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k.trim() == name).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    let realm = field("realm")?;
    let nonce = field("nonce")?;
    let qop = field("qop");
    let ha1 = md5_hex(&format!("{user}:{realm}:{pass}"));
    let ha2 = md5_hex(&format!("{method}:{uri}"));
    let cnonce = uuid::Uuid::new_v4().simple().to_string();
    let nc = "00000001";
    Some(match qop {
        Some(q) if q.split(',').any(|x| x.trim() == "auth") => {
            let response = md5_hex(&format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}"));
            format!(r#"Digest username="{user}", realm="{realm}", nonce="{nonce}", uri="{uri}", qop=auth, nc={nc}, cnonce="{cnonce}", response="{response}""#)
        }
        _ => {
            let response = md5_hex(&format!("{ha1}:{nonce}:{ha2}"));
            format!(r#"Digest username="{user}", realm="{realm}", nonce="{nonce}", uri="{uri}", response="{response}""#)
        }
    })
}

/// One entry in a playlist sent to the player channel.
pub struct Item {
    pub url: String,
    pub title: String,
    pub fmt: &'static str,
    pub subtitles: Option<String>,
    /// false = load paused, for starting several devices together.
    pub autoplay: bool,
}

/// "video", "audio" or "image" for a Roku stream format.
pub fn kind(fmt: &str) -> &'static str {
    if fmt == "image" { "image" } else if fmt.starts_with("audio:") { "audio" } else { "video" }
}

/// Roku's streamFormat for a file, or None if the TV can't play it.
pub fn stream_format(path: &std::path::Path) -> Option<&'static str> {
    format_for_ext(path.extension()?.to_str()?)
}

/// Stream format from a URL's path, defaulting to MP4 when there's no hint.
pub fn stream_format_for_url(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit_once('.').and_then(|(_, ext)| format_for_ext(ext)).unwrap_or("mp4")
}

/// Subtitle file next to a video: same name with .srt or .vtt (any case).
pub fn sidecar_subtitles(video: &std::path::Path) -> Option<std::path::PathBuf> {
    let stem = video.file_stem()?.to_str()?.to_lowercase();
    let dir = video.parent()?;
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
        let ext = p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
        let same = p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_lowercase()) == Some(stem.clone());
        same && matches!(ext.as_deref(), Some("srt" | "vtt"))
    })
}

fn format_for_ext(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" | "mov" => Some("mp4"),
        "mkv" => Some("mkv"),
        "ts" => Some("ts"),
        "webm" => Some("mp4"),
        "m3u8" => Some("hls"),
        "jpg" | "jpeg" | "png" | "gif" | "bmp" => Some("image"),
        "mp3" => Some("audio:mp3"),
        "m4a" => Some("audio:mp4"),
        "aac" => Some("audio:es.aac-adts"),
        "flac" => Some("audio:flac"),
        "wav" => Some("audio:wav"),
        _ => None,
    }
}

fn q(s: &str) -> String {
    s.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

/// Start a playlist in the player channel (hand it over if it's already open).
pub async fn play(ip: &str, items: &[Item]) -> Result<(), String> {
    let c = client();
    let active = c.get(format!("http://{ip}:8060/query/active-app")).send().await
        .map_err(|e| e.to_string())?.text().await.unwrap_or_default();
    let mut params = format!("n={}", items.len());
    if items.first().map(|i| !i.autoplay).unwrap_or(false) {
        params += "&ap=0";
    }
    for (i, it) in items.iter().enumerate() {
        let n = i + 1;
        params += &format!("&u{n}={}&t{n}={}&f{n}={}", q(&it.url), q(&it.title), it.fmt);
        if let Some(s) = &it.subtitles {
            params += &format!("&s{n}={}", q(s));
        }
    }
    let endpoint = if active.contains(r#"id="dev""#) {
        format!("http://{ip}:8060/input?{params}")
    } else {
        format!("http://{ip}:8060/launch/dev?{params}")
    };
    let r = c.post(&endpoint).send().await.map_err(|e| e.to_string())?;
    if r.status().is_success() {
        info!(ip, items = items.len(), first = %items.first().map(|i| i.url.as_str()).unwrap_or(""), "roku player: play");
        Ok(())
    } else if r.status() == reqwest::StatusCode::NOT_FOUND {
        Err("The player channel isn't installed on this TV. Use Set up video playback first.".into())
    } else {
        Err(format!("The TV refused to start playback ({}).", r.status()))
    }
}

/// Show a picture that refreshes every `every` seconds (the calendar). With `save`,
/// the TV also keeps the address for its screensaver.
pub async fn show_calendar(ip: &str, url: &str, every: u64, save: bool) -> Result<(), String> {
    let c = client();
    let active = c.get(format!("http://{ip}:8060/query/active-app")).send().await
        .map_err(|e| e.to_string())?.text().await.unwrap_or_default();
    let params = format!("cal={}&every={every}{}", q(url), if save { "&save=1" } else { "" });
    let endpoint = if active.contains(r#"id="dev""#) {
        format!("http://{ip}:8060/input?{params}")
    } else {
        format!("http://{ip}:8060/launch/dev?{params}")
    };
    let r = c.post(&endpoint).send().await.map_err(|e| e.to_string())?;
    if r.status().is_success() {
        info!(ip, save, "roku player: calendar");
        Ok(())
    } else if r.status() == reqwest::StatusCode::NOT_FOUND {
        Err("The player channel isn't installed on this TV. Use Set up video playback first.".into())
    } else {
        Err(format!("The TV refused ({}).", r.status()))
    }
}

/// Version of the channel bundled in this build ("major.minor.build").
pub fn bundled_version() -> String {
    let get = |k: &str| MANIFEST.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).unwrap_or("0").trim().to_string();
    format!("{}.{}.{}", get("major_version"), get("minor_version"), get("build_version"))
}

/// Version of the player channel installed on the TV, if any.
pub async fn installed_version(ip: &str) -> Option<String> {
    let apps = client().get(format!("http://{ip}:8060/query/apps")).send().await.ok()?.text().await.ok()?;
    let chunk = apps.split("<app ").find(|c| c.contains(r#"id="dev""#))?;
    let start = chunk.find(r#"version=""#)? + 9;
    let end = chunk[start..].find('"')? + start;
    Some(chunk[start..end].to_string())
}

fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// True when the TV has an older player channel than this build carries.
pub async fn needs_upgrade(ip: &str) -> bool {
    match installed_version(ip).await {
        Some(v) => version_key(&v) < version_key(&bundled_version()),
        None => false,
    }
}
