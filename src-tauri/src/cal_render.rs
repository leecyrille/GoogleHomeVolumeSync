//! Draws the calendar for TVs inside the app.
//!
//! A hidden, off-screen webview shows public/calendar (the PactoTech Calendar
//! Saver page in TV mode). Just after each minute it is saved as a picture for
//! every view (month, week, day) and theme in use, and in 4K mode also as a
//! 4K video of that picture, because Roku apps can only draw pictures at 1080p.

use crate::core::Core;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tracing::{info, warn};

pub const VIEWS: [&str; 3] = ["month", "week", "day"];
const LABEL: &str = "calendar-tv";
/// Each 4K video shows its picture this long (1 frame a second: Roku players stall on a single-frame file); a new one replaces it every minute.
const VIDEO_SECS: u32 = 75;
const PHOTO_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];
/// Where the webviews reach the calphoto protocol.
#[cfg(windows)]
const PHOTO_BASE: &str = "http://calphoto.localhost";
#[cfg(not(windows))]
const PHOTO_BASE: &str = "calphoto://localhost";

pub fn dir() -> PathBuf {
    let d = crate::config::config_dir().join("calendar");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// One look of the calendar: theme, text size and whether photos show. Each look in
/// use gets its own set of pictures.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Variant {
    pub theme: String,
    pub text_pct: u32,
    pub photos: bool,
}

impl Variant {
    /// e.g. "dark-t100-p1" (the Roku player puts this in the file names it fetches).
    pub fn name(&self) -> String {
        format!("{}-t{}-p{}", self.theme, self.text_pct, self.photos as u8)
    }
}

fn is_variant_name(s: &str) -> bool {
    let mut it = s.split('-');
    let (Some(theme), Some(t), Some(p), None) = (it.next(), it.next(), it.next(), it.next()) else { return false };
    matches!(theme, "dark" | "light")
        && t.strip_prefix('t').map(|n| !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit())).unwrap_or(false)
        && matches!(p, "p0" | "p1")
}

pub fn file_name(variant: &str, view: &str, ext: &str) -> String {
    format!("calendar-{variant}-{view}.{ext}")
}

/// Files TVs may fetch from the calendar folder.
pub fn is_served_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("calendar-") else { return false };
    let Some((stem, ext)) = rest.rsplit_once('.') else { return false };
    let Some((variant, view)) = stem.rsplit_once('-') else { return false };
    is_variant_name(variant) && VIEWS.contains(&view) && matches!(ext, "jpg" | "mp4")
}

#[derive(Serialize, Clone, Default)]
pub struct RenderStatus {
    pub rendering: bool,
    /// When the latest pictures were made (unix ms).
    pub updated: Option<i64>,
    pub error: Option<String>,
    pub feeds: Vec<crate::cal_feeds::FeedStatus>,
    pub events: usize,
    pub photos: usize,
}

#[derive(Default)]
struct State {
    variants: BTreeSet<Variant>,
    four_k: bool,
    kick: bool,
    reload_data: bool,
    events_json: String,
    feeds: Vec<crate::cal_feeds::FeedStatus>,
    event_count: usize,
    photos: Vec<String>,
    last_fetch: Option<Instant>,
    last_scan: Option<Instant>,
    last_refresh_label: Option<String>,
    window_4k: Option<bool>,
    status: RenderStatus,
}

fn st() -> MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap()
}

/// Which looks TVs need now (empty = nothing to draw), and whether 4K videos are wanted.
pub fn want(variants: BTreeSet<Variant>, four_k: bool) {
    let mut s = st();
    if s.variants != variants || s.four_k != four_k {
        let new_look = variants.iter().any(|t| !s.variants.contains(t)) || (four_k && !s.four_k);
        s.variants = variants;
        s.four_k = four_k;
        if new_look {
            s.kick = true;
        }
    }
}

/// Draw again now (e.g. settings changed).
pub fn kick(reload_data: bool) {
    let mut s = st();
    s.kick = true;
    if reload_data {
        s.reload_data = true;
    }
}

pub fn status() -> RenderStatus {
    st().status.clone()
}

fn unix_ms(t: SystemTime) -> i64 {
    t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

// ---------------- data: feeds and photos ----------------

async fn refresh_data(core: &Core, force: bool) {
    let cfg = core.inner.lock().unwrap().cfg.calendar.clone();
    let (fetch_due, scan_due, first) = {
        let s = st();
        let refresh = Duration::from_secs(cfg.refresh_minutes.max(1) as u64 * 60);
        (
            force || s.last_fetch.map(|t| t.elapsed() >= refresh).unwrap_or(true),
            force || s.last_scan.map(|t| t.elapsed() >= Duration::from_secs(30 * 60)).unwrap_or(true),
            s.last_fetch.is_none(),
        )
    };
    if fetch_due {
        let feeds: Vec<crate::cal_feeds::FeedCfg> = cfg.feeds.clone();
        let cache = crate::config::config_dir().join("calendar-cache");
        let _ = std::fs::create_dir_all(&cache);
        let today = chrono::Local::now().date_naive();
        // The first time, show the cached feeds straight away; the network result follows.
        if first {
            let r = crate::cal_feeds::fetch_all(&feeds, &cache, true, today).await;
            store_events(r, None);
        }
        let r = crate::cal_feeds::fetch_all(&feeds, &cache, false, today).await;
        let label = r.network.then(|| chrono::Local::now().format("%H:%M").to_string());
        store_events(r, label);
        st().last_fetch = Some(Instant::now());
    }
    if scan_due {
        let photos = scan_photos(&cfg.photo_folders);
        let mut s = st();
        s.photos = photos;
        s.last_scan = Some(Instant::now());
    }
}

fn store_events(r: crate::cal_feeds::FetchResult, label: Option<String>) {
    let mut s = st();
    s.events_json = serde_json::to_string(&r.events).unwrap_or_else(|_| "[]".into());
    s.event_count = r.events.len();
    s.feeds = r.statuses;
    if label.is_some() {
        s.last_refresh_label = label;
    }
}

/// Photo addresses the page loads through the calphoto protocol.
fn scan_photos(folders: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, root) in folders.iter().enumerate() {
        let root = Path::new(root);
        if !root.is_dir() {
            continue;
        }
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                let hidden = e.file_name().to_string_lossy().starts_with('.');
                if hidden {
                    continue;
                }
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|x| x.to_str()).map(|x| PHOTO_EXTS.contains(&x.to_ascii_lowercase().as_str())).unwrap_or(false) {
                    if let Ok(rel) = p.strip_prefix(root) {
                        let parts: Vec<String> = rel.components().map(|c| encode(&c.as_os_str().to_string_lossy())).collect();
                        out.push(format!("{PHOTO_BASE}/p/{i}/{}", parts.join("/")));
                    }
                }
            }
        }
    }
    out
}

fn encode(s: &str) -> String {
    s.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// calphoto://  (http://calphoto.localhost on Windows)
///   /p/<folder>/<path>   a photo from one of the calendar's photo folders
///   /preview/<file>      a finished calendar picture, for the app's preview
pub fn protocol(app: &tauri::AppHandle, request: tauri::http::Request<Vec<u8>>, responder: tauri::UriSchemeResponder) {
    let path = decode(request.uri().path());
    let not_found = || tauri::http::Response::builder().status(404).body(Vec::new()).unwrap();
    let file = if let Some(rest) = path.strip_prefix("/p/") {
        let (idx, rel) = rest.split_once('/').unwrap_or((rest, ""));
        let folders = app.state::<Arc<Core>>().inner.lock().unwrap().cfg.calendar.photo_folders.clone();
        idx.parse::<usize>().ok().and_then(|i| folders.get(i).cloned()).and_then(|root| {
            let root = std::fs::canonicalize(root).ok()?;
            let rel = if cfg!(windows) { rel.replace('/', "\\") } else { rel.to_string() };
            let full = std::fs::canonicalize(root.join(rel)).ok()?;
            full.starts_with(&root).then_some(full)
        })
    } else if let Some(name) = path.strip_prefix("/preview/") {
        is_served_name(name).then(|| dir().join(name))
    } else {
        None
    };
    let Some(file) = file else { return responder.respond(not_found()) };
    std::thread::spawn(move || {
        let resp = match std::fs::read(&file) {
            Ok(bytes) => tauri::http::Response::builder()
                .header("Content-Type", crate::media_server::content_type(&file))
                .header("Cache-Control", "no-store")
                .body(bytes).unwrap(),
            Err(_) => tauri::http::Response::builder().status(404).body(Vec::new()).unwrap(),
        };
        responder.respond(resp);
    });
}

// ---------------- the off-screen webview ----------------

async fn ensure_window(app: &tauri::AppHandle, four_k: bool) -> Result<tauri::WebviewWindow, String> {
    if let Some(w) = app.get_webview_window(LABEL) {
        if st().window_4k == Some(four_k) {
            return Ok(w);
        }
        let _ = w.destroy();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let (w, h) = if four_k { (3840u32, 2160u32) } else { (1920, 1080) };
    let builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("calendar/index.html".into()))
        .title("Calendar (for TVs)")
        .decorations(false)
        .resizable(false)
        .skip_taskbar(true)
        .focused(false)
        .shadow(false)
        .inner_size(w as f64, h as f64)
        .initialization_script("window.__TV__ = true;")
        .data_directory(crate::config::config_dir().join("calendar-webview"));
    // Past the right edge of every monitor: it renders, but nobody sees it.
    #[cfg(windows)]
    let right = app.available_monitors().ok().unwrap_or_default().iter()
        .map(|m| m.position().x + m.size().width as i32).max().unwrap_or(1920);
    #[cfg(windows)]
    let builder = builder
        .visible(true)
        .position((right + 400) as f64, 0.0)
        // Off-screen windows count as hidden to Chromium, which would stop painting them.
        .additional_browser_args("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion --disable-backgrounding-occluded-windows --disable-renderer-backgrounding");
    // Never shown: its webview moves into a GTK off-screen window (offscreen::attach).
    #[cfg(not(windows))]
    let builder = builder.visible(false);
    let window = builder.build().map_err(|e| format!("Couldn't open the calendar drawing window: {e}"))?;
    #[cfg(windows)]
    {
        let _ = window.set_size(tauri::PhysicalSize::new(w, h));
        let _ = window.set_position(tauri::PhysicalPosition::new(right + 400, 0));
        hide_from_alt_tab(&window);
        // One CSS pixel per 1080p pixel, whatever the display scaling: 4K draws the same page at double detail.
        let scale = window.scale_factor().unwrap_or(1.0);
        let _ = window.set_zoom((if four_k { 2.0 } else { 1.0 }) / scale);
    }
    #[cfg(target_os = "linux")]
    offscreen::attach(&window, w, h, four_k).await?;
    st().window_4k = Some(four_k);
    info!(four_k, "calendar: drawing window opened");
    // Page, fonts and first photos.
    tokio::time::sleep(Duration::from_secs(4)).await;
    Ok(window)
}

#[cfg(windows)]
fn hide_from_alt_tab(window: &tauri::WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW};
    if let Ok(hwnd) = window.hwnd() {
        unsafe {
            let hwnd = windows::Win32::Foundation::HWND(hwnd.0 as _);
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let ex = (ex | WS_EX_TOOLWINDOW.0 as isize | WS_EX_NOACTIVATE.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
        }
    }
}

fn close_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) {
        #[cfg(target_os = "linux")]
        offscreen::detach(app);
        let _ = w.destroy();
        st().window_4k = None;
        info!("calendar: drawing window closed");
    }
}

#[cfg(windows)]
type Reply = tokio::sync::oneshot::Sender<Result<Vec<u8>, String>>;

/// Save what the webview shows as JPEG or PNG bytes.
#[cfg(windows)]
async fn capture(window: &tauri::WebviewWindow, png: bool) -> Result<Vec<u8>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx: Arc<Mutex<Option<Reply>>> = Arc::new(Mutex::new(Some(tx)));
    let tx2 = tx.clone();
    window.with_webview(move |pw| {
        if let Err(e) = unsafe { start_capture(pw, png, tx2.clone()) } {
            if let Some(t) = tx2.lock().unwrap().take() {
                let _ = t.send(Err(e));
            }
        }
    }).map_err(|e| e.to_string())?;
    match tokio::time::timeout(Duration::from_secs(20), rx).await {
        Ok(Ok(r)) => r,
        _ => Err("The calendar picture took too long.".into()),
    }
}

#[cfg(windows)]
unsafe fn start_capture(pw: tauri::webview::PlatformWebview, png: bool, tx: Arc<Mutex<Option<Reply>>>) -> Result<(), String> {
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    let core = pw.controller().CoreWebView2().map_err(|e| e.to_string())?;
    let stream = windows::Win32::UI::Shell::SHCreateMemStream(None).ok_or("Couldn't make a picture buffer.")?;
    let format = if png { COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG } else { COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_JPEG };
    let done_stream = stream.clone();
    let handler = webview2_com::CapturePreviewCompletedHandler::create(Box::new(move |result| {
        let out = result.map_err(|e| e.to_string()).and_then(|_| read_stream(&done_stream));
        if let Some(t) = tx.lock().unwrap().take() {
            let _ = t.send(out);
        }
        Ok(())
    }));
    core.CapturePreview(format, &stream, &handler).map_err(|e| e.to_string())
}

#[cfg(windows)]
unsafe fn read_stream(stream: &windows::Win32::System::Com::IStream) -> Result<Vec<u8>, String> {
    use windows::Win32::System::Com::{STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};
    let mut stat = STATSTG::default();
    stream.Stat(&mut stat, STATFLAG_NONAME).map_err(|e| e.to_string())?;
    stream.Seek(0, STREAM_SEEK_SET, None).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; stat.cbSize as usize];
    let mut read = 0u32;
    stream.Read(buf.as_mut_ptr() as *mut _, buf.len() as u32, Some(&mut read)).ok().map_err(|e| e.to_string())?;
    buf.truncate(read as usize);
    Ok(buf)
}

/// Linux: WebKitGTK draws the calendar inside a GTK off-screen window, so it keeps laying
/// out and painting without appearing on any screen (Wayland can't place a window past the
/// edge of the screen the way Windows can).
#[cfg(target_os = "linux")]
mod offscreen {
    use gtk::prelude::*;
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use webkit2gtk::{SnapshotOptions, SnapshotRegion, WebViewExt};

    thread_local! {
        // GTK objects live on the main thread only.
        static HOST: RefCell<Option<gtk::OffscreenWindow>> = const { RefCell::new(None) };
    }

    /// Move the hidden calendar window's webview into an off-screen window of `w`x`h` pixels.
    pub async fn attach(window: &tauri::WebviewWindow, w: u32, h: u32, four_k: bool) -> Result<(), String> {
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        window.with_webview(move |pw| {
            let view = pw.inner();
            if let Some(parent) = view.parent().and_then(|p| p.downcast::<gtk::Container>().ok()) {
                parent.remove(&view);
            }
            let host = gtk::OffscreenWindow::new();
            // One CSS pixel per 1080p pixel, whatever the display scaling: 4K draws the same page at double detail.
            let scale = host.scale_factor().max(1);
            let (lw, lh) = (w as i32 / scale, h as i32 / scale);
            host.set_default_size(lw, lh);
            view.set_size_request(lw, lh);
            host.add(&view);
            host.show_all();
            view.set_zoom_level((if four_k { 2.0 } else { 1.0 }) / scale as f64);
            if let Some(old) = HOST.with(|c| c.borrow_mut().replace(host)) {
                old.close();
            }
            let _ = tx.send(Ok(()));
        }).map_err(|e| e.to_string())?;
        match tokio::time::timeout(Duration::from_secs(10), rx).await {
            Ok(Ok(r)) => r,
            _ => Err("The calendar drawing window didn't start.".into()),
        }
    }

    pub fn detach(app: &tauri::AppHandle) {
        let _ = app.run_on_main_thread(|| {
            if let Some(host) = HOST.with(|c| c.borrow_mut().take()) {
                host.close();
            }
        });
    }

    type Reply = tokio::sync::oneshot::Sender<Result<(u32, u32, Vec<u8>), String>>;

    /// What the webview shows, as tightly packed BGRA.
    pub async fn snapshot(window: &tauri::WebviewWindow) -> Result<(u32, u32, Vec<u8>), String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx: Arc<Mutex<Option<Reply>>> = Arc::new(Mutex::new(Some(tx)));
        window.with_webview(move |pw| {
            pw.inner().snapshot(SnapshotRegion::Visible, SnapshotOptions::NONE, None::<&webkit2gtk::gio::Cancellable>, move |r| {
                let out = r.map_err(|e| e.to_string()).and_then(|s| to_bgra(&s));
                if let Some(t) = tx.lock().unwrap().take() {
                    let _ = t.send(out);
                }
            });
        }).map_err(|e| e.to_string())?;
        match tokio::time::timeout(Duration::from_secs(20), rx).await {
            Ok(Ok(r)) => r,
            _ => Err("The calendar picture took too long.".into()),
        }
    }

    fn to_bgra(surface: &cairo::Surface) -> Result<(u32, u32, Vec<u8>), String> {
        let err = |e: cairo::Error| format!("Couldn't read the calendar picture: {e}");
        let (x0, y0, x1, y1) = cairo::Context::new(surface).and_then(|c| c.clip_extents()).map_err(err)?;
        let (w, h) = ((x1 - x0).round() as i32, (y1 - y0).round() as i32);
        if w <= 0 || h <= 0 {
            return Err("The calendar picture came out empty.".into());
        }
        // Copy onto a surface we own, so its pixels can be read.
        let mut img = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).map_err(err)?;
        {
            let cr = cairo::Context::new(&img).map_err(err)?;
            cr.set_source_surface(surface, -x0, -y0).map_err(err)?;
            cr.paint().map_err(err)?;
        }
        img.flush();
        let stride = img.stride() as usize;
        let data = img.data().map_err(|e| format!("Couldn't read the calendar picture: {e}"))?;
        // Cairo's ARGB32 is B, G, R, A in memory on little-endian machines.
        let mut out = Vec::with_capacity(w as usize * h as usize * 4);
        for row in data.chunks(stride).take(h as usize) {
            for px in row[..w as usize * 4].chunks_exact(4) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
        Ok((w as u32, h as u32, out))
    }
}

/// Save what the webview shows as JPEG or PNG bytes.
#[cfg(target_os = "linux")]
async fn capture(window: &tauri::WebviewWindow, png: bool) -> Result<Vec<u8>, String> {
    let (w, h, bgra) = offscreen::snapshot(window).await?;
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let mut out = std::io::Cursor::new(Vec::new());
        if png {
            let rgba: Vec<u8> = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
            let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("Bad calendar picture size.")?;
            img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
        } else {
            let rgb: Vec<u8> = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
            let img = image::RgbImage::from_raw(w, h, rgb).ok_or("Bad calendar picture size.")?;
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92).encode_image(&img).map_err(|e| e.to_string())?;
        }
        Ok(out.into_inner())
    }).await.map_err(|e| e.to_string())?
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    for attempt in 0..5 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(_) if attempt < 4 => std::thread::sleep(Duration::from_millis(150)),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

fn payload(v: &Variant, view: &str, interval: u32) -> String {
    let s = st();
    let now = chrono::Local::now();
    let feeds = serde_json::to_string(&s.feeds).unwrap_or_else(|_| "[]".into());
    let photos = if v.photos { serde_json::to_string(&s.photos).unwrap_or_else(|_| "[]".into()) } else { "[]".into() };
    format!(
        r#"{{"type":"data","year":{},"month":{},"events":{},"tasks":[],"feeds":{},"photos":{},"photoIntervalSeconds":{},"lastRefresh":{},"theme":"{}","view":"{}","textScale":{}}}"#,
        chrono::Datelike::year(&now), chrono::Datelike::month(&now), s.events_json, feeds, photos,
        interval, serde_json::to_string(&s.last_refresh_label).unwrap_or_else(|_| "null".into()), v.theme, view, v.text_pct as f64 / 100.0
    )
}

/// Draw every view for each theme in use and save the pictures (and 4K videos).
async fn draw_all(app: &tauri::AppHandle, core: &Core) -> Result<(), String> {
    let (variants, four_k) = {
        let s = st();
        (s.variants.clone(), s.four_k)
    };
    let window = ensure_window(app, four_k).await?;
    let interval = core.inner.lock().unwrap().cfg.calendar.photo_interval_secs.max(5);
    let out = dir();
    for variant in &variants {
        let name = variant.name();
        let data = payload(variant, "month", interval);
        window.eval(&format!("window.__tv && window.__tv.apply({data})")).map_err(|e| e.to_string())?;
        for view in VIEWS {
            window.eval(&format!("window.__tv && (window.__tv.view('{view}'), window.__tv.tick())")).map_err(|e| e.to_string())?;
            tokio::time::sleep(Duration::from_millis(400)).await;
            let jpg = capture(&window, false).await?;
            write_atomic(&out.join(file_name(&name, view, "jpg")), &jpg)?;
            if four_k {
                let png = capture(&window, true).await?;
                let target = out.join(file_name(&name, view, "mp4"));
                tokio::task::spawn_blocking(move || -> Result<(), String> {
                    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).map_err(|e| e.to_string())?.to_rgba8();
                    let (w, h) = img.dimensions();
                    let mut bgra = img.into_raw();
                    for px in bgra.chunks_exact_mut(4) {
                        px.swap(0, 2);
                    }
                    crate::cal_video::encode_still_mp4_frames(&bgra, w & !1, h & !1, VIDEO_SECS, 1, &target)
                }).await.map_err(|e| e.to_string())??;
            }
        }
    }
    // Looks no screen uses any more.
    if let Ok(entries) = std::fs::read_dir(&out) {
        for e in entries.flatten() {
            let stale = e.metadata().and_then(|m| m.modified()).ok()
                .and_then(|t| t.elapsed().ok()).map(|age| age > Duration::from_secs(2 * 3600)).unwrap_or(false);
            if stale {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// Keeps the pictures fresh while TVs need them; closes the webview when none do.
pub async fn run(app: tauri::AppHandle, core: Arc<Core>) {
    let mut last_minute: Option<i64> = None;
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (wanted, kicked, reload) = {
            let mut s = st();
            let r = (!s.variants.is_empty(), s.kick, s.reload_data);
            s.kick = false;
            s.reload_data = false;
            r
        };
        if !wanted {
            if st().window_4k.is_some() {
                close_window(&app);
                st().status.rendering = false;
                core.emit_state();
            }
            last_minute = None;
            continue;
        }
        // Just after each minute, so the clock in the picture is right.
        let now = chrono::Local::now();
        let minute = now.timestamp() / 60;
        let due = last_minute != Some(minute) && chrono::Timelike::second(&now) >= 1;
        if !due && !kicked {
            continue;
        }
        last_minute = Some(minute);
        refresh_data(&core, reload).await;
        let result = draw_all(&app, &core).await;
        {
            let mut s = st();
            s.status.rendering = true;
            s.status.feeds = s.feeds.clone();
            s.status.events = s.event_count;
            s.status.photos = s.photos.len();
            match &result {
                Ok(()) => {
                    s.status.updated = Some(unix_ms(SystemTime::now()));
                    s.status.error = None;
                }
                Err(e) => {
                    warn!(error=%e, "calendar: drawing failed");
                    s.status.error = Some(e.clone());
                }
            }
        }
        crate::calendar::pictures_ready(&core).await;
        core.emit_state();
    }
}
