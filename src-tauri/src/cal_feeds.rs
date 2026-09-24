//! Reads iCalendar (ICS) feeds and expands them into display events for the visible month.
//! Port of the PactoTech Calendar Saver's C# FeedService (Ical.Net). Tasks (VTODO) are ignored.

use chrono::{
    DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone,
    Timelike, Utc, Weekday,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

fn default_color() -> String {
    "#7aa2f7".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FeedCfg {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventOut {
    pub feed: usize,
    pub title: String,
    pub all_day: bool,
    pub start: String,
    pub end: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct FeedStatus {
    pub name: String,
    pub color: String,
    pub stale: bool,
    pub error: Option<String>,
    pub enabled: bool,
}

#[derive(Clone, Debug)]
pub struct FetchResult {
    pub events: Vec<EventOut>,
    pub statuses: Vec<FeedStatus>,
    /// True when the network was tried (not cache-only), like the C# LastNetworkRefresh.
    pub network: bool,
}

// ---------------------------------------------------------------------------------------------
// Fetching and caching
// ---------------------------------------------------------------------------------------------

/// Fetches every feed and expands events over the visible window (month -7 days .. next month +7 days).
/// With cache_only, only the on-disk cache is read; otherwise the network is tried first and the
/// cache is the fallback (the feed is then marked stale).
pub async fn fetch_all(
    feeds: &[FeedCfg],
    cache_dir: &Path,
    cache_only: bool,
    today: NaiveDate,
) -> FetchResult {
    let (window_start, window_end) = visible_window(today);
    let mut events = Vec::new();
    let mut statuses = Vec::new();

    for (i, feed) in feeds.iter().enumerate() {
        let name = if feed.name.trim().is_empty() {
            format!("Feed {}", i + 1)
        } else {
            feed.name.trim().to_string()
        };
        if !feed.enabled || feed.url.trim().is_empty() {
            // Keep a status entry so event feed indexes stay aligned with the feeds list.
            statuses.push(FeedStatus { name, color: feed.color.clone(), stale: false, error: None, enabled: false });
            continue;
        }

        let mut ics: Option<String> = None;
        let mut stale = false;
        let mut error: Option<String> = None;
        let cache_path = cache_path_for(cache_dir, &feed.url);

        if !cache_only {
            match fetch_text(&feed.url).await {
                Ok(text) => {
                    let _ = tokio::fs::create_dir_all(cache_dir).await;
                    if let Err(e) = tokio::fs::write(&cache_path, text.as_bytes()).await {
                        error = Some(shorten(&e.to_string()));
                    }
                    ics = Some(text);
                }
                Err(e) => error = Some(shorten(&e)),
            }
        }

        if ics.is_none() && tokio::fs::try_exists(&cache_path).await.unwrap_or(false) {
            match read_text_file(&cache_path).await {
                Ok(text) => {
                    ics = Some(text);
                    stale = !cache_only;
                }
                Err(e) => {
                    if error.is_none() {
                        error = Some(shorten(&e));
                    }
                }
            }
        }

        if let Some(text) = ics {
            match events_from_ics(&text, i, window_start, window_end) {
                Ok(mut list) => events.append(&mut list),
                Err(e) => error = Some(format!("parse: {}", shorten(&e))),
            }
        }

        statuses.push(FeedStatus { name, color: feed.color.clone(), stale, error, enabled: true });
    }

    events.sort_by(|a, b| a.start.cmp(&b.start).then(a.feed.cmp(&b.feed)).then(a.title.cmp(&b.title)));
    FetchResult { events, statuses, network: !cache_only }
}

/// Validates a feed URL with a live fetch. Ok carries e.g. "12 events"; Err carries the reason.
pub async fn test_feed(url: &str) -> Result<String, String> {
    if is_google_share_link(url) {
        return Err("this is a Google share link, not an ICS feed \u{2014} in Google Calendar, right-click \
the calendar in the left sidebar \u{2192} \u{201C}Settings and sharing\u{201D}, scroll to the bottom, \
and copy the \u{201C}Secret address in iCal format\u{201D} (ends in basic.ics)"
            .to_string());
    }
    let text = fetch_text(url).await.map_err(|e| shorten(&e))?;
    if !contains_ci(&text, "BEGIN:VCALENDAR") {
        return Err("not an ICS file".to_string());
    }
    Ok(format!("{} events", count_ci(&text, "BEGIN:VEVENT")))
}

/// Where a feed's raw ICS is cached: feed_<first 16 hex chars of SHA-256(trimmed url)>.ics
pub fn cache_path_for(cache_dir: &Path, url: &str) -> PathBuf {
    let hash = Sha256::digest(url.trim().as_bytes());
    let hex: String = hash.iter().take(8).map(|b| format!("{b:02X}")).collect();
    cache_dir.join(format!("feed_{hex}.ics"))
}

fn visible_window(today: NaiveDate) -> (NaiveDate, NaiveDate) {
    let month_start = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
    let (ny, nm) = if today.month() == 12 { (today.year() + 1, 1) } else { (today.year(), today.month() + 1) };
    let next_month = NaiveDate::from_ymd_opt(ny, nm, 1).unwrap_or(today);
    (month_start - Duration::days(7), next_month + Duration::days(7))
}

fn is_google_share_link(url: &str) -> bool {
    let u = url.trim();
    contains_ci(u, "calendar.google.com") && !contains_ci(u, "/ical/")
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_ascii_uppercase().contains(&needle.to_ascii_uppercase())
}

fn count_ci(hay: &str, needle: &str) -> usize {
    hay.to_ascii_uppercase().matches(&needle.to_ascii_uppercase()).count()
}

fn shorten(msg: &str) -> String {
    if msg.chars().count() > 120 {
        let mut s: String = msg.chars().take(120).collect();
        s.push('\u{2026}');
        s
    } else {
        msg.to_string()
    }
}

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(25))
            .user_agent("CalendarSaver/1.0")
            // No idle pooling: refreshes are minutes apart, and pooled connections are tied to one runtime.
            .pool_max_idle_per_host(0)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

async fn fetch_text(url: &str) -> Result<String, String> {
    let mut u = url.trim().to_string();
    if starts_with_ci(&u, "webcal://") {
        u = format!("https://{}", &u["webcal://".len()..]);
    }
    if starts_with_ci(&u, "file://") {
        return read_text_file(&file_url_to_path(&u)).await;
    }
    if !u.is_empty() && Path::new(&u).is_file() {
        return read_text_file(Path::new(&u)).await;
    }

    let resp = http().get(&u).send().await.map_err(net_error)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!(
            "Response status code does not indicate success: {} ({}).",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        ));
    }
    let bytes = resp.bytes().await.map_err(net_error)?;
    Ok(decode_text(&bytes))
}

fn net_error(e: reqwest::Error) -> String {
    if e.is_timeout() {
        return "The request was canceled due to the configured HttpClient.Timeout of 25 seconds elapsing."
            .to_string();
    }
    if e.is_builder() {
        return "An invalid request URI was provided.".to_string();
    }
    // Innermost cause, like the C# InnerException message; never includes the (secret) URL.
    let e = e.without_url();
    let mut src: &dyn std::error::Error = &e;
    while let Some(next) = src.source() {
        src = next;
    }
    src.to_string()
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.is_char_boundary(prefix.len()) && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn file_url_to_path(u: &str) -> PathBuf {
    let rest = &u["file://".len()..];
    let decoded = percent_decode(rest);
    let bytes = decoded.as_bytes();
    // file:///C:/x -> C:/x ; file://server/share -> \\server\share
    if bytes.first() == Some(&b'/') {
        let r = &decoded[1..];
        let rb = r.as_bytes();
        if rb.len() >= 2 && rb[1] == b':' {
            return PathBuf::from(r.replace('/', "\\"));
        }
        return PathBuf::from(format!("/{r}"));
    }
    if bytes.len() >= 2 && bytes[1] == b':' {
        return PathBuf::from(decoded.replace('/', "\\"));
    }
    PathBuf::from(format!("\\\\{}", decoded.replace('/', "\\")))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn read_text_file(path: &Path) -> Result<String, String> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(decode_text(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("Could not find file '{}'.", path.display()))
        }
        Err(e) => Err(e.to_string()),
    }
}

fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

// ---------------------------------------------------------------------------------------------
// ICS parsing
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn param(&self, key: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug)]
struct Comp {
    name: String,
    props: Vec<Prop>,
    children: Vec<Comp>,
}

impl Comp {
    fn prop(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name == name)
    }

    fn props<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Prop> + 'a {
        self.props.iter().filter(move |p| p.name == name)
    }
}

/// Joins folded lines (a line starting with space or tab continues the previous one).
fn unfold(text: &str) -> Vec<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if (line.starts_with(' ') || line.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(&line[1..]);
        } else if !line.is_empty() {
            lines.push(line.to_string());
        }
    }
    lines
}

/// Splits "NAME;P1=a,b;P2=\"x:y\":value" into name, params and value.
fn parse_line(line: &str) -> Option<Prop> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i] != b';' && b[i] != b':' {
        i += 1;
    }
    if i >= b.len() {
        return None;
    }
    let name = line[..i].trim().to_ascii_uppercase();
    let mut params = Vec::new();
    while i < b.len() && b[i] == b';' {
        i += 1;
        let ks = i;
        while i < b.len() && !matches!(b[i], b'=' | b';' | b':') {
            i += 1;
        }
        let key = line[ks..i].trim().to_ascii_uppercase();
        let mut vals: Vec<&str> = Vec::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            loop {
                if i < b.len() && b[i] == b'"' {
                    i += 1;
                    let vs = i;
                    while i < b.len() && b[i] != b'"' {
                        i += 1;
                    }
                    vals.push(&line[vs..i]);
                    if i < b.len() {
                        i += 1;
                    }
                    while i < b.len() && !matches!(b[i], b',' | b';' | b':') {
                        i += 1;
                    }
                } else {
                    let vs = i;
                    while i < b.len() && !matches!(b[i], b',' | b';' | b':') {
                        i += 1;
                    }
                    vals.push(&line[vs..i]);
                }
                if i < b.len() && b[i] == b',' {
                    i += 1;
                    continue;
                }
                break;
            }
        }
        params.push((key, vals.join(",")));
    }
    if i >= b.len() || b[i] != b':' {
        return None;
    }
    Some(Prop { name, params, value: line[i + 1..].to_string() })
}

fn parse_components(text: &str) -> Result<Vec<Comp>, String> {
    let mut stack: Vec<Comp> = Vec::new();
    let mut roots: Vec<Comp> = Vec::new();
    fn attach(c: Comp, stack: &mut [Comp], roots: &mut Vec<Comp>) {
        match stack.last_mut() {
            Some(parent) => parent.children.push(c),
            None => roots.push(c),
        }
    }
    for line in unfold(text) {
        let Some(p) = parse_line(&line) else { continue };
        if p.name == "BEGIN" {
            stack.push(Comp { name: p.value.trim().to_ascii_uppercase(), props: Vec::new(), children: Vec::new() });
        } else if p.name == "END" {
            let n = p.value.trim().to_ascii_uppercase();
            if let Some(pos) = stack.iter().rposition(|c| c.name == n) {
                while stack.len() > pos {
                    let c = stack.pop().unwrap();
                    attach(c, &mut stack, &mut roots);
                }
            }
        } else if let Some(top) = stack.last_mut() {
            top.props.push(p);
        }
    }
    while let Some(c) = stack.pop() {
        attach(c, &mut stack, &mut roots);
    }
    if !roots.iter().any(|c| c.name == "VCALENDAR") {
        return Err("not an ICS file".to_string());
    }
    Ok(roots)
}

fn collect_vevents<'a>(comps: &'a [Comp], out: &mut Vec<&'a Comp>) {
    for c in comps {
        if c.name == "VEVENT" {
            out.push(c);
        } else if c.name != "VTODO" && c.name != "VJOURNAL" && c.name != "VTIMEZONE" {
            collect_vevents(&c.children, out);
        }
    }
}

fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(x) => out.push(x),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Date-time values and time zones
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Zone {
    /// No TZID and no Z: already machine-local (also used for unknown TZIDs, like Ical.Net).
    Floating,
    Utc,
    Named(chrono_tz::Tz),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum IcsTime {
    Date(NaiveDate),
    Time(NaiveDateTime, Zone),
}

fn parse_time_value(v: &str, tzid: Option<&str>, is_date: bool) -> Option<IcsTime> {
    let v = v.trim();
    if is_date || !v.contains(['T', 't']) {
        let d = v.get(..8)?;
        return NaiveDate::parse_from_str(d, "%Y%m%d").ok().map(IcsTime::Date);
    }
    let (body, utc) = match v.strip_suffix(['Z', 'z']) {
        Some(b) => (b, true),
        None => (v, false),
    };
    let ndt = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M"))
        .ok()?;
    let zone = if utc {
        Zone::Utc
    } else if let Some(t) = tzid.filter(|t| !t.trim().is_empty()) {
        resolve_tzid(t)
    } else {
        Zone::Floating
    };
    Some(IcsTime::Time(ndt, zone))
}

/// All values of a property (comma-separated lists; PERIOD values give their start and optional end).
fn prop_times(p: &Prop) -> Vec<(IcsTime, Option<IcsTime>, Option<Duration>)> {
    let value_type = p.param("VALUE").unwrap_or("").to_ascii_uppercase();
    let is_date = value_type == "DATE";
    let tzid = p.param("TZID");
    let mut out = Vec::new();
    for part in p.value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (start_s, end_s) = match part.split_once('/') {
            Some((a, b)) => (a, Some(b)),
            None => (part, None),
        };
        let Some(start) = parse_time_value(start_s, tzid, is_date) else { continue };
        let mut end = None;
        let mut dur = None;
        if let Some(e) = end_s {
            if e.trim_start_matches(['+', '-']).starts_with(['P', 'p']) {
                dur = parse_duration(e);
            } else {
                end = parse_time_value(e, tzid, is_date);
            }
        }
        out.push((start, end, dur));
    }
    out
}

fn first_time(p: &Prop) -> Option<IcsTime> {
    prop_times(p).into_iter().next().map(|(t, _, _)| t)
}

/// ISO 8601 / RFC 5545 duration such as "PT1H30M", "-P1D" or "P2W".
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix(['P', 'p'])?;
    let mut total = Duration::zero();
    let mut num = String::new();
    let mut any = false;
    for c in s.chars() {
        match c.to_ascii_uppercase() {
            'T' => {}
            d if d.is_ascii_digit() => num.push(d),
            unit => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                any = true;
                total += match unit {
                    'W' => Duration::weeks(n),
                    'D' => Duration::days(n),
                    'H' => Duration::hours(n),
                    'M' => Duration::minutes(n),
                    'S' => Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    if !any {
        return None;
    }
    Some(if neg { -total } else { total })
}

static WINDOWS_ZONES: &[(&str, &str)] = &[
    ("Dateline Standard Time", "Etc/GMT+12"),
    ("UTC-11", "Etc/GMT+11"),
    ("Aleutian Standard Time", "America/Adak"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("Marquesas Standard Time", "Pacific/Marquesas"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("UTC-09", "Etc/GMT+9"),
    ("Pacific Standard Time (Mexico)", "America/Tijuana"),
    ("UTC-08", "Etc/GMT+8"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("Mountain Standard Time (Mexico)", "America/Mazatlan"),
    ("Mountain Standard Time", "America/Denver"),
    ("Yukon Standard Time", "America/Whitehorse"),
    ("Central America Standard Time", "America/Guatemala"),
    ("Central Standard Time", "America/Chicago"),
    ("Easter Island Standard Time", "Pacific/Easter"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("Canada Central Standard Time", "America/Regina"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("Eastern Standard Time (Mexico)", "America/Cancun"),
    ("Eastern Standard Time", "America/New_York"),
    ("Haiti Standard Time", "America/Port-au-Prince"),
    ("Cuba Standard Time", "America/Havana"),
    ("US Eastern Standard Time", "America/Indiana/Indianapolis"),
    ("Turks And Caicos Standard Time", "America/Grand_Turk"),
    ("Paraguay Standard Time", "America/Asuncion"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("Venezuela Standard Time", "America/Caracas"),
    ("Central Brazilian Standard Time", "America/Cuiaba"),
    ("SA Western Standard Time", "America/La_Paz"),
    ("Pacific SA Standard Time", "America/Santiago"),
    ("Newfoundland Standard Time", "America/St_Johns"),
    ("Tocantins Standard Time", "America/Araguaina"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("SA Eastern Standard Time", "America/Cayenne"),
    ("Argentina Standard Time", "America/Argentina/Buenos_Aires"),
    ("Greenland Standard Time", "America/Nuuk"),
    ("Montevideo Standard Time", "America/Montevideo"),
    ("Magallanes Standard Time", "America/Punta_Arenas"),
    ("Saint Pierre Standard Time", "America/Miquelon"),
    ("Bahia Standard Time", "America/Bahia"),
    ("UTC-02", "Etc/GMT+2"),
    ("Mid-Atlantic Standard Time", "Etc/GMT+2"),
    ("Azores Standard Time", "Atlantic/Azores"),
    ("Cape Verde Standard Time", "Atlantic/Cape_Verde"),
    ("UTC", "Etc/UTC"),
    ("Coordinated Universal Time", "Etc/UTC"),
    ("GMT Standard Time", "Europe/London"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("Sao Tome Standard Time", "Africa/Sao_Tome"),
    ("Morocco Standard Time", "Africa/Casablanca"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("Jordan Standard Time", "Asia/Amman"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Middle East Standard Time", "Asia/Beirut"),
    ("Egypt Standard Time", "Africa/Cairo"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("Syria Standard Time", "Asia/Damascus"),
    ("West Bank Standard Time", "Asia/Hebron"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("FLE Standard Time", "Europe/Kyiv"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("South Sudan Standard Time", "Africa/Juba"),
    ("Kaliningrad Standard Time", "Europe/Kaliningrad"),
    ("Sudan Standard Time", "Africa/Khartoum"),
    ("Libya Standard Time", "Africa/Tripoli"),
    ("Namibia Standard Time", "Africa/Windhoek"),
    ("Arabic Standard Time", "Asia/Baghdad"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("Arab Standard Time", "Asia/Riyadh"),
    ("Belarus Standard Time", "Europe/Minsk"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("Volgograd Standard Time", "Europe/Volgograd"),
    ("Iran Standard Time", "Asia/Tehran"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Astrakhan Standard Time", "Europe/Astrakhan"),
    ("Azerbaijan Standard Time", "Asia/Baku"),
    ("Russia Time Zone 3", "Europe/Samara"),
    ("Mauritius Standard Time", "Indian/Mauritius"),
    ("Saratov Standard Time", "Europe/Saratov"),
    ("Georgian Standard Time", "Asia/Tbilisi"),
    ("Caucasus Standard Time", "Asia/Yerevan"),
    ("Afghanistan Standard Time", "Asia/Kabul"),
    ("West Asia Standard Time", "Asia/Tashkent"),
    ("Ekaterinburg Standard Time", "Asia/Yekaterinburg"),
    ("Pakistan Standard Time", "Asia/Karachi"),
    ("Qyzylorda Standard Time", "Asia/Qyzylorda"),
    ("India Standard Time", "Asia/Kolkata"),
    ("Sri Lanka Standard Time", "Asia/Colombo"),
    ("Nepal Standard Time", "Asia/Kathmandu"),
    ("Central Asia Standard Time", "Asia/Bishkek"),
    ("Bangladesh Standard Time", "Asia/Dhaka"),
    ("Omsk Standard Time", "Asia/Omsk"),
    ("Myanmar Standard Time", "Asia/Yangon"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("Altai Standard Time", "Asia/Barnaul"),
    ("W. Mongolia Standard Time", "Asia/Hovd"),
    ("North Asia Standard Time", "Asia/Krasnoyarsk"),
    ("N. Central Asia Standard Time", "Asia/Novosibirsk"),
    ("Tomsk Standard Time", "Asia/Tomsk"),
    ("China Standard Time", "Asia/Shanghai"),
    ("North Asia East Standard Time", "Asia/Irkutsk"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Ulaanbaatar Standard Time", "Asia/Ulaanbaatar"),
    ("Aus Central W. Standard Time", "Australia/Eucla"),
    ("Transbaikal Standard Time", "Asia/Chita"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("North Korea Standard Time", "Asia/Pyongyang"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Yakutsk Standard Time", "Asia/Yakutsk"),
    ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("AUS Central Standard Time", "Australia/Darwin"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("West Pacific Standard Time", "Pacific/Port_Moresby"),
    ("Tasmania Standard Time", "Australia/Hobart"),
    ("Vladivostok Standard Time", "Asia/Vladivostok"),
    ("Lord Howe Standard Time", "Australia/Lord_Howe"),
    ("Bougainville Standard Time", "Pacific/Bougainville"),
    ("Russia Time Zone 10", "Asia/Srednekolymsk"),
    ("Magadan Standard Time", "Asia/Magadan"),
    ("Norfolk Standard Time", "Pacific/Norfolk"),
    ("Sakhalin Standard Time", "Asia/Sakhalin"),
    ("Central Pacific Standard Time", "Pacific/Guadalcanal"),
    ("Russia Time Zone 11", "Asia/Kamchatka"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("UTC+12", "Etc/GMT-12"),
    ("Fiji Standard Time", "Pacific/Fiji"),
    ("Chatham Islands Standard Time", "Pacific/Chatham"),
    ("UTC+13", "Etc/GMT-13"),
    ("Tonga Standard Time", "Pacific/Tongatapu"),
    ("Samoa Standard Time", "Pacific/Apia"),
    ("Line Islands Standard Time", "Pacific/Kiritimati"),
];

/// Maps a TZID to a zone: IANA name, Windows name, then the same fallbacks as Ical.Net
/// (dash-to-slash, a known IANA name inside the text). Unknown ids are treated as local time.
fn resolve_tzid(tzid: &str) -> Zone {
    static CACHE: OnceLock<Mutex<HashMap<String, Zone>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(z) = cache.lock().ok().and_then(|m| m.get(tzid).copied()) {
        return z;
    }
    let z = resolve_tzid_uncached(tzid);
    if let Ok(mut m) = cache.lock() {
        m.insert(tzid.to_string(), z);
    }
    z
}

fn resolve_tzid_uncached(tzid: &str) -> Zone {
    let t = tzid.trim().trim_matches('"');
    let t = t.strip_prefix('/').unwrap_or(t);
    if let Ok(tz) = t.parse::<chrono_tz::Tz>() {
        return Zone::Named(tz);
    }
    if let Some((_, iana)) = WINDOWS_ZONES.iter().find(|(w, _)| w.eq_ignore_ascii_case(t)) {
        if let Ok(tz) = iana.parse::<chrono_tz::Tz>() {
            return Zone::Named(tz);
        }
    }
    if let Some(tz) = chrono_tz::TZ_VARIANTS.iter().find(|z| z.name().eq_ignore_ascii_case(t)) {
        return Zone::Named(*tz);
    }
    if let Ok(tz) = t.replace('-', "/").parse::<chrono_tz::Tz>() {
        return Zone::Named(tz);
    }
    if let Some(tz) = chrono_tz::TZ_VARIANTS
        .iter()
        .filter(|z| z.name().len() > 3 && t.contains(z.name()))
        .max_by_key(|z| z.name().len())
    {
        return Zone::Named(*tz);
    }
    if let Some((_, iana)) = WINDOWS_ZONES
        .iter()
        .filter(|(w, _)| w.len() > 3 && t.contains(w))
        .max_by_key(|(w, _)| w.len())
    {
        if let Ok(tz) = iana.parse::<chrono_tz::Tz>() {
            return Zone::Named(tz);
        }
    }
    Zone::Floating
}

/// Wall-clock time in a zone to an absolute instant. Repeated times take the earlier one;
/// skipped times (spring-forward gap) move forward by the gap, like NodaTime's lenient resolver.
fn resolve_in<Z: TimeZone>(tz: &Z, ndt: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&ndt) {
        LocalResult::Single(d) => d.with_timezone(&Utc),
        LocalResult::Ambiguous(a, _) => a.with_timezone(&Utc),
        LocalResult::None => {
            let before = tz.from_local_datetime(&(ndt - Duration::hours(3))).earliest();
            match before {
                Some(b) => {
                    let offset = b.naive_local() - b.naive_utc();
                    Utc.from_utc_datetime(&(ndt - offset))
                }
                None => Utc.from_utc_datetime(&ndt),
            }
        }
    }
}

fn to_utc(ndt: NaiveDateTime, zone: Zone) -> DateTime<Utc> {
    match zone {
        Zone::Floating => resolve_in(&Local, ndt),
        Zone::Utc => Utc.from_utc_datetime(&ndt),
        Zone::Named(tz) => resolve_in(&tz, ndt),
    }
}

fn from_utc(t: DateTime<Utc>, zone: Zone) -> NaiveDateTime {
    match zone {
        Zone::Floating => t.with_timezone(&Local).naive_local(),
        Zone::Utc => t.naive_utc(),
        Zone::Named(tz) => t.with_timezone(&tz).naive_local(),
    }
}

fn to_local(ndt: NaiveDateTime, zone: Zone) -> NaiveDateTime {
    match zone {
        Zone::Floating => ndt,
        _ => from_utc(to_utc(ndt, zone), Zone::Floating),
    }
}

/// Re-expresses a time as wall-clock time in another zone.
fn convert_wall(ndt: NaiveDateTime, from: Zone, to: Zone) -> NaiveDateTime {
    if from == to { ndt } else { from_utc(to_utc(ndt, from), to) }
}

// ---------------------------------------------------------------------------------------------
// RRULE expansion
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
enum Freq {
    Secondly,
    Minutely,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Clone, Debug)]
struct RRule {
    freq: Freq,
    interval: i64,
    count: Option<u32>,
    until: Option<IcsTime>,
    by_second: Vec<u32>,
    by_minute: Vec<u32>,
    by_hour: Vec<u32>,
    by_day: Vec<(i32, Weekday)>,
    by_month_day: Vec<i32>,
    by_year_day: Vec<i32>,
    by_week_no: Vec<i32>,
    by_month: Vec<u32>,
    by_set_pos: Vec<i32>,
    wkst: Weekday,
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    Some(match s.to_ascii_uppercase().as_str() {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

fn parse_rrule(v: &str) -> Option<RRule> {
    let mut r = RRule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        by_second: vec![],
        by_minute: vec![],
        by_hour: vec![],
        by_day: vec![],
        by_month_day: vec![],
        by_year_day: vec![],
        by_week_no: vec![],
        by_month: vec![],
        by_set_pos: vec![],
        wkst: Weekday::Mon,
    };
    let mut have_freq = false;
    fn ints<T: std::str::FromStr>(s: &str) -> Vec<T> {
        s.split(',').filter_map(|x| x.trim().trim_start_matches('+').parse().ok()).collect()
    }
    for part in v.trim().split(';') {
        let Some((k, val)) = part.split_once('=') else { continue };
        let val = val.trim();
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                r.freq = match val.to_ascii_uppercase().as_str() {
                    "SECONDLY" => Freq::Secondly,
                    "MINUTELY" => Freq::Minutely,
                    "HOURLY" => Freq::Hourly,
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return None,
                };
                have_freq = true;
            }
            "INTERVAL" => r.interval = val.parse().unwrap_or(1).max(1),
            "COUNT" => r.count = val.parse().ok(),
            "UNTIL" => r.until = parse_time_value(val, None, false),
            "BYSECOND" => r.by_second = ints(val),
            "BYMINUTE" => r.by_minute = ints(val),
            "BYHOUR" => r.by_hour = ints(val),
            "BYMONTHDAY" => r.by_month_day = ints(val),
            "BYYEARDAY" => r.by_year_day = ints(val),
            "BYWEEKNO" => r.by_week_no = ints(val),
            "BYMONTH" => r.by_month = ints(val),
            "BYSETPOS" => r.by_set_pos = ints(val),
            "WKST" => r.wkst = parse_weekday(val).unwrap_or(Weekday::Mon),
            "BYDAY" => {
                for item in val.split(',') {
                    let item = item.trim();
                    if item.len() < 2 {
                        continue;
                    }
                    let (num, wd) = item.split_at(item.len() - 2);
                    let Some(wd) = parse_weekday(wd) else { continue };
                    let n = if num.is_empty() || num == "+" { 0 } else { num.trim_start_matches('+').parse().unwrap_or(0) };
                    r.by_day.push((n, wd));
                }
            }
            _ => {}
        }
    }
    have_freq.then_some(r)
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    match (NaiveDate::from_ymd_opt(ny, nm, 1), NaiveDate::from_ymd_opt(y, m, 1)) {
        (Some(a), Some(b)) => (a - b).num_days() as u32,
        _ => 30,
    }
}

fn days_in_year(y: i32) -> u32 {
    if NaiveDate::from_ymd_opt(y, 2, 29).is_some() { 366 } else { 365 }
}

/// First day of week 1 of a year: the first week (starting on wkst) with at least 4 days in that year.
fn week1_start(y: i32, wkst: Weekday) -> NaiveDate {
    let jan1 = NaiveDate::from_ymd_opt(y, 1, 1).unwrap_or(NaiveDate::MIN);
    let offset = (jan1.weekday().num_days_from_monday() + 7 - wkst.num_days_from_monday()) % 7;
    if offset <= 3 {
        jan1 - Duration::days(offset as i64)
    } else {
        jan1 + Duration::days((7 - offset) as i64)
    }
}

struct DayFilter<'a> {
    freq: Freq,
    by_month: &'a [u32],
    by_week_no: &'a [i32],
    by_year_day: &'a [i32],
    by_month_day: &'a [i32],
    by_day: &'a [(i32, Weekday)],
    wkst: Weekday,
}

impl DayFilter<'_> {
    fn matches(&self, d: NaiveDate) -> bool {
        if !self.by_month.is_empty() && !self.by_month.contains(&d.month()) {
            return false;
        }
        if !self.by_week_no.is_empty() {
            let mut wy = d.year();
            if d >= week1_start(wy + 1, self.wkst) {
                wy += 1;
            } else if d < week1_start(wy, self.wkst) {
                wy -= 1;
            }
            let start = week1_start(wy, self.wkst);
            let week = ((d - start).num_days() / 7 + 1) as i32;
            let weeks = ((week1_start(wy + 1, self.wkst) - start).num_days() / 7) as i32;
            if !self.by_week_no.iter().any(|&n| n == week || (n < 0 && weeks + n + 1 == week)) {
                return false;
            }
        }
        if !self.by_year_day.is_empty() {
            let yd = d.ordinal() as i32;
            let ny = days_in_year(d.year()) as i32;
            if !self.by_year_day.iter().any(|&n| n == yd || (n < 0 && ny + n + 1 == yd)) {
                return false;
            }
        }
        if !self.by_month_day.is_empty() {
            let md = d.day() as i32;
            let dim = days_in_month(d.year(), d.month()) as i32;
            if !self.by_month_day.iter().any(|&n| n == md || (n < 0 && dim + n + 1 == md)) {
                return false;
            }
        }
        if !self.by_day.is_empty() {
            let ordinal_ok = |n: i32| -> bool {
                if n == 0 || !matches!(self.freq, Freq::Monthly | Freq::Yearly) {
                    return true;
                }
                // Nth weekday of the month (MONTHLY, or YEARLY with BYMONTH), else of the year.
                let (pos, len) = if self.freq == Freq::Monthly || !self.by_month.is_empty() {
                    (d.day() as i32, days_in_month(d.year(), d.month()) as i32)
                } else {
                    (d.ordinal() as i32, days_in_year(d.year()) as i32)
                };
                let fwd = (pos - 1) / 7 + 1;
                let back = -((len - pos) / 7 + 1);
                n == fwd || n == back
            };
            if !self.by_day.iter().any(|&(n, wd)| wd == d.weekday() && ordinal_ok(n)) {
                return false;
            }
        }
        true
    }
}

/// Expands an RRULE in wall-clock time from dtstart. Returns occurrences >= dtstart and <= limit,
/// honouring COUNT and UNTIL (already converted to the same wall clock).
fn expand_rrule(
    r: &RRule,
    dtstart: NaiveDateTime,
    until: Option<NaiveDateTime>,
    limit: NaiveDateTime,
) -> Vec<NaiveDateTime> {
    let mut out = Vec::new();
    if dtstart > limit {
        return out;
    }
    let d0 = dtstart.date();
    let mut by_day = r.by_day.clone();
    let mut by_month_day = r.by_month_day.clone();
    let mut by_month = r.by_month.clone();
    if by_day.is_empty() && by_month_day.is_empty() && r.by_year_day.is_empty() && r.by_week_no.is_empty() {
        match r.freq {
            Freq::Yearly => {
                if by_month.is_empty() {
                    by_month = vec![d0.month()];
                }
                by_month_day = vec![d0.day() as i32];
            }
            Freq::Monthly => by_month_day = vec![d0.day() as i32],
            Freq::Weekly => by_day = vec![(0, d0.weekday())],
            _ => {}
        }
    }
    let filter = DayFilter {
        freq: r.freq,
        by_month: &by_month,
        by_week_no: &r.by_week_no,
        by_year_day: &r.by_year_day,
        by_month_day: &by_month_day,
        by_day: &by_day,
        wkst: r.wkst,
    };
    let sorted = |v: &[u32], dflt: u32| -> Vec<u32> {
        let mut v: Vec<u32> = if v.is_empty() { vec![dflt] } else { v.to_vec() };
        v.sort_unstable();
        v.dedup();
        v
    };
    let hours = sorted(&r.by_hour, dtstart.hour());
    let minutes = sorted(&r.by_minute, dtstart.minute());
    let seconds = sorted(&r.by_second, dtstart.second());
    let mut times = Vec::new();
    for &h in &hours {
        for &m in &minutes {
            for &s in &seconds {
                if let Some(t) = NaiveTime::from_hms_opt(h, m, s) {
                    times.push(t);
                }
            }
        }
    }

    let interval = r.interval.max(1);
    let wk_off = (d0.weekday().num_days_from_monday() + 7 - r.wkst.num_days_from_monday()) % 7;
    let week0 = d0 - Duration::days(wk_off as i64);
    let sub_daily_origin = match r.freq {
        Freq::Hourly => dtstart.date().and_hms_opt(dtstart.hour(), 0, 0).unwrap_or(dtstart),
        Freq::Minutely => dtstart.date().and_hms_opt(dtstart.hour(), dtstart.minute(), 0).unwrap_or(dtstart),
        _ => dtstart,
    };
    let mut emitted: u32 = 0;
    let mut cands: Vec<NaiveDateTime> = Vec::new();
    let mut days: Vec<NaiveDate> = Vec::new();

    for k in 0..500_000i64 {
        cands.clear();
        days.clear();
        let first_day: NaiveDate;
        match r.freq {
            Freq::Yearly => {
                let y = d0.year() as i64 + k * interval;
                if y > 9999 {
                    break;
                }
                let y = y as i32;
                first_day = NaiveDate::from_ymd_opt(y, 1, 1).unwrap();
                for m in 1..=12u32 {
                    if !by_month.is_empty() && !by_month.contains(&m) {
                        continue;
                    }
                    for dd in 1..=days_in_month(y, m) {
                        days.push(NaiveDate::from_ymd_opt(y, m, dd).unwrap());
                    }
                }
            }
            Freq::Monthly => {
                let idx = d0.year() as i64 * 12 + (d0.month() as i64 - 1) + k * interval;
                let (y, m) = ((idx.div_euclid(12)) as i32, (idx.rem_euclid(12) + 1) as u32);
                if y > 9999 {
                    break;
                }
                first_day = NaiveDate::from_ymd_opt(y, m, 1).unwrap();
                for dd in 1..=days_in_month(y, m) {
                    days.push(NaiveDate::from_ymd_opt(y, m, dd).unwrap());
                }
            }
            Freq::Weekly => {
                first_day = week0 + Duration::days(7 * interval * k);
                for i in 0..7 {
                    days.push(first_day + Duration::days(i));
                }
            }
            Freq::Daily => {
                first_day = d0 + Duration::days(interval * k);
                days.push(first_day);
            }
            Freq::Hourly | Freq::Minutely | Freq::Secondly => {
                let step = match r.freq {
                    Freq::Hourly => Duration::hours(interval * k),
                    Freq::Minutely => Duration::minutes(interval * k),
                    _ => Duration::seconds(interval * k),
                };
                let p = sub_daily_origin + step;
                first_day = p.date();
                if filter.matches(p.date())
                    && (r.by_hour.is_empty() || r.by_hour.contains(&p.hour()))
                    && (r.freq == Freq::Hourly || r.by_minute.is_empty() || r.by_minute.contains(&p.minute()))
                    && (r.freq != Freq::Secondly || r.by_second.is_empty() || r.by_second.contains(&p.second()))
                {
                    let mins: Vec<u32> = if r.freq == Freq::Hourly { minutes.clone() } else { vec![p.minute()] };
                    let secs: Vec<u32> = if r.freq == Freq::Secondly { vec![p.second()] } else { seconds.clone() };
                    for &m in &mins {
                        for &s in &secs {
                            if let Some(t) = p.date().and_hms_opt(p.hour(), m, s) {
                                cands.push(t);
                            }
                        }
                    }
                }
            }
        }
        let period_start = first_day.and_time(NaiveTime::MIN);
        if period_start > limit + Duration::days(1) || until.is_some_and(|u| period_start > u + Duration::days(1)) {
            break;
        }
        for &d in &days {
            if filter.matches(d) {
                for &t in &times {
                    cands.push(d.and_time(t));
                }
            }
        }
        cands.sort_unstable();
        cands.dedup();
        if !r.by_set_pos.is_empty() {
            let n = cands.len() as i32;
            let mut sel: Vec<NaiveDateTime> = r
                .by_set_pos
                .iter()
                .filter_map(|&p| {
                    let idx = if p > 0 { p - 1 } else { n + p };
                    (idx >= 0 && idx < n).then(|| cands[idx as usize])
                })
                .collect();
            sel.sort_unstable();
            sel.dedup();
            cands = sel;
        }
        for &c in &cands {
            if c < dtstart {
                continue;
            }
            if until.is_some_and(|u| c > u) || c > limit {
                return out;
            }
            out.push(c);
            emitted += 1;
            if r.count.is_some_and(|n| emitted >= n) {
                return out;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Dur {
    /// All-day length in days (DTEND is exclusive).
    Days(i64),
    /// Added to the wall-clock start (keeps 9:00-10:00 across DST changes).
    Wall(Duration),
    /// Exact elapsed time (used when DTSTART and DTEND are in different zones).
    Abs(Duration),
}

fn event_timing(c: &Comp) -> Option<(IcsTime, Dur)> {
    let start = c.prop("DTSTART").and_then(first_time)?;
    let end = c.prop("DTEND").and_then(first_time);
    let dur = c.prop("DURATION").and_then(|p| parse_duration(&p.value));
    let d = match start {
        IcsTime::Date(sd) => match (end, dur) {
            (Some(IcsTime::Date(ed)), _) => Dur::Days((ed - sd).num_days()),
            (Some(IcsTime::Time(edt, _)), _) => Dur::Days((edt.date() - sd).num_days()),
            (None, Some(d)) => Dur::Days(d.num_days()),
            (None, None) => Dur::Days(1),
        },
        IcsTime::Time(sdt, sz) => match (end, dur) {
            (Some(IcsTime::Time(edt, ez)), _) => {
                if ez == sz {
                    Dur::Wall(edt - sdt)
                } else {
                    Dur::Abs(to_utc(edt, ez) - to_utc(sdt, sz))
                }
            }
            (Some(IcsTime::Date(ed)), _) => Dur::Wall(ed.and_time(NaiveTime::MIN) - sdt),
            (None, Some(d)) => Dur::Wall(d),
            (None, None) => Dur::Wall(Duration::zero()),
        },
    };
    Some((start, d))
}

fn title_of(c: &Comp) -> String {
    let t = c.prop("SUMMARY").map(|p| unescape_text(&p.value)).unwrap_or_default();
    let t = t.trim();
    if t.is_empty() { "(untitled)".to_string() } else { t.to_string() }
}

fn is_cancelled(c: &Comp) -> bool {
    c.prop("STATUS").is_some_and(|p| p.value.trim().eq_ignore_ascii_case("CANCELLED"))
}

struct Window {
    start: NaiveDateTime,
    end: NaiveDateTime,
}

impl Window {
    fn overlaps(&self, s: NaiveDateTime, e: NaiveDateTime) -> bool {
        (e > self.start && s < self.end) || (s == e && s == self.start)
    }
}

const TS_FMT: &str = "%Y-%m-%dT%H:%M:%S";

/// Emits one occurrence. For all-day events `wall` is local midnight of the start date.
fn emit(out: &mut Vec<EventOut>, feed: usize, title: &str, wall: NaiveDateTime, zone: Zone, all_day: bool, dur: Dur, win: &Window) {
    if all_day {
        let s = wall.date();
        let days = match dur {
            Dur::Days(n) => n,
            Dur::Wall(d) | Dur::Abs(d) => d.num_days(),
        };
        let end_excl = s + Duration::days(days);
        let (sd, ed) = (s.and_time(NaiveTime::MIN), end_excl.and_time(NaiveTime::MIN));
        if !win.overlaps(sd, ed.max(sd)) {
            return;
        }
        let e = if end_excl > s { end_excl - Duration::days(1) } else { s };
        out.push(EventOut {
            feed,
            title: title.to_string(),
            all_day: true,
            start: s.format("%Y-%m-%d").to_string(),
            end: e.format("%Y-%m-%d").to_string(),
        });
    } else {
        let start = to_local(wall, zone);
        let end = match dur {
            Dur::Wall(d) => to_local(wall + d, zone),
            Dur::Days(n) => to_local(wall + Duration::days(n), zone),
            Dur::Abs(d) => from_utc(to_utc(wall, zone) + d, Zone::Floating),
        };
        if !win.overlaps(start, end) {
            return;
        }
        out.push(EventOut {
            feed,
            title: title.to_string(),
            all_day: false,
            start: start.format(TS_FMT).to_string(),
            end: end.format(TS_FMT).to_string(),
        });
    }
}

/// Converts any ICS time to wall-clock time in the series' zone (all-day series use local midnight).
fn to_series_wall(t: IcsTime, zone: Zone, series_time: NaiveTime, all_day: bool) -> NaiveDateTime {
    match t {
        IcsTime::Date(d) => d.and_time(if all_day { NaiveTime::MIN } else { series_time }),
        IcsTime::Time(ndt, z) => {
            let w = convert_wall(ndt, z, zone);
            if all_day { w.date().and_time(NaiveTime::MIN) } else { w }
        }
    }
}

fn expand_series(c: &Comp, overrides: &[(IcsTime, &Comp)], feed: usize, win: &Window, out: &mut Vec<EventOut>) {
    if is_cancelled(c) {
        return;
    }
    let Some((start, dur)) = event_timing(c) else { return };
    let title = title_of(c);
    let (wall0, zone, all_day) = match start {
        IcsTime::Date(d) => (d.and_time(NaiveTime::MIN), Zone::Floating, true),
        IcsTime::Time(ndt, z) => (ndt, z, false),
    };
    let series_time = wall0.time();
    // Window end in the series' wall clock, with slack for zone offsets.
    let limit = win.end + Duration::days(2);

    let mut starts: Vec<(NaiveDateTime, Dur)> = vec![(wall0, dur)];
    for p in c.props("RRULE") {
        let Some(rule) = parse_rrule(&p.value) else { continue };
        let until = rule.until.map(|u| match u {
            // A date-only UNTIL includes that whole day.
            IcsTime::Date(d) => d.and_hms_opt(23, 59, 59).unwrap(),
            IcsTime::Time(ndt, z) => convert_wall(ndt, z, zone),
        });
        for t in expand_rrule(&rule, wall0, until, limit) {
            starts.push((t, dur));
        }
    }
    for p in c.props("RDATE") {
        for (t, end, pdur) in prop_times(p) {
            let w = to_series_wall(t, zone, series_time, all_day);
            let d = match (end, pdur) {
                (Some(e), _) => {
                    let ew = to_series_wall(e, zone, series_time, all_day);
                    if all_day { Dur::Days((ew.date() - w.date()).num_days()) } else { Dur::Wall(ew - w) }
                }
                (None, Some(pd)) => if all_day { Dur::Days(pd.num_days()) } else { Dur::Wall(pd) },
                (None, None) => dur,
            };
            starts.push((w, d));
        }
    }
    starts.sort_by_key(|(s, _)| *s);
    starts.dedup_by_key(|(s, _)| *s);

    let mut skip_exact: HashSet<NaiveDateTime> = HashSet::new();
    let mut skip_dates: HashSet<NaiveDate> = HashSet::new();
    for p in c.props("EXDATE") {
        for (t, _, _) in prop_times(p) {
            match t {
                IcsTime::Date(d) => {
                    skip_dates.insert(d);
                }
                IcsTime::Time(..) => {
                    let w = to_series_wall(t, zone, series_time, all_day);
                    if all_day {
                        skip_dates.insert(w.date());
                    } else {
                        skip_exact.insert(w);
                    }
                }
            }
        }
    }
    // Overridden instances (RECURRENCE-ID) are replaced by the override event.
    let instance_set: HashSet<NaiveDateTime> = starts.iter().map(|(s, _)| *s).collect();
    for (rid, _) in overrides {
        match rid {
            IcsTime::Date(d) => {
                skip_dates.insert(*d);
            }
            IcsTime::Time(..) => {
                let w = to_series_wall(*rid, zone, series_time, all_day);
                if all_day {
                    skip_dates.insert(w.date());
                } else if instance_set.contains(&w) {
                    skip_exact.insert(w);
                } else {
                    // No exact match (e.g. the series time was edited later): replace that day's instance.
                    skip_dates.insert(w.date());
                }
            }
        }
    }

    for (s, d) in starts {
        if skip_exact.contains(&s) || skip_dates.contains(&s.date()) {
            continue;
        }
        emit(out, feed, &title, s, zone, all_day, d, win);
    }
}

fn emit_single(c: &Comp, feed: usize, win: &Window, out: &mut Vec<EventOut>) {
    if is_cancelled(c) {
        return;
    }
    let Some((start, dur)) = event_timing(c) else { return };
    let title = title_of(c);
    match start {
        IcsTime::Date(d) => emit(out, feed, &title, d.and_time(NaiveTime::MIN), Zone::Floating, true, dur, win),
        IcsTime::Time(ndt, z) => emit(out, feed, &title, ndt, z, false, dur, win),
    }
}

/// Parses one ICS document and returns its event occurrences overlapping [window_start, window_end).
fn events_from_ics(
    text: &str,
    feed: usize,
    window_start: NaiveDate,
    window_end: NaiveDate,
) -> Result<Vec<EventOut>, String> {
    let roots = parse_components(text)?;
    let mut vevents = Vec::new();
    collect_vevents(&roots, &mut vevents);
    let win = Window { start: window_start.and_time(NaiveTime::MIN), end: window_end.and_time(NaiveTime::MIN) };

    let mut series: Vec<(String, &Comp)> = Vec::new();
    let mut overrides: HashMap<String, Vec<(IcsTime, &Comp)>> = HashMap::new();
    for c in vevents {
        let uid = c.prop("UID").map(|p| p.value.trim().to_string()).unwrap_or_default();
        match c.prop("RECURRENCE-ID").and_then(first_time) {
            Some(rid) => overrides.entry(uid).or_default().push((rid, c)),
            None => series.push((uid, c)),
        }
    }

    let mut out = Vec::new();
    for (uid, c) in &series {
        let ovs = overrides.get(uid).map(|v| v.as_slice()).unwrap_or(&[]);
        expand_series(c, ovs, feed, &win, &mut out);
    }
    for list in overrides.values() {
        for (_, c) in list {
            emit_single(c, feed, &win, &mut out);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cal(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{}\r\nEND:VCALENDAR\r\n", body.trim().replace('\n', "\r\n"))
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 23).unwrap()
    }

    fn run(body: &str) -> Vec<EventOut> {
        let (ws, we) = visible_window(today());
        let mut v = events_from_ics(&cal(body), 0, ws, we).unwrap();
        v.sort_by(|a, b| a.start.cmp(&b.start));
        v
    }

    fn starts(v: &[EventOut]) -> Vec<String> {
        v.iter().map(|e| e.start.clone()).collect()
    }

    /// Expected local rendering of a wall time in an IANA zone.
    fn loc(tz: &str, s: &str) -> String {
        let tz: chrono_tz::Tz = tz.parse().unwrap();
        let ndt = NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S").unwrap();
        tz.from_local_datetime(&ndt).unwrap().with_timezone(&Local).naive_local().format(TS_FMT).to_string()
    }

    fn utc_loc(s: &str) -> String {
        let ndt = NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S").unwrap();
        Utc.from_utc_datetime(&ndt).with_timezone(&Local).naive_local().format(TS_FMT).to_string()
    }

    #[test]
    fn window_is_month_plus_minus_week() {
        let (s, e) = visible_window(today());
        assert_eq!(s.to_string(), "2026-08-25");
        assert_eq!(e.to_string(), "2026-10-08");
        let (s, e) = visible_window(NaiveDate::from_ymd_opt(2026, 12, 5).unwrap());
        assert_eq!(s.to_string(), "2026-11-24");
        assert_eq!(e.to_string(), "2027-01-08");
    }

    #[test]
    fn weekly_byday_with_exdate() {
        let v = run("BEGIN:VEVENT
UID:w1
DTSTART;TZID=America/Edmonton:20260907T090000
DTEND;TZID=America/Edmonton:20260907T100000
RRULE:FREQ=WEEKLY;WKST=SU;UNTIL=20261001T055959Z;BYDAY=MO,WE
EXDATE;TZID=America/Edmonton:20260916T090000
SUMMARY:Standup
END:VEVENT");
        let want: Vec<String> = ["20260907", "20260909", "20260914", "20260921", "20260923", "20260928", "20260930"]
            .iter()
            .map(|d| loc("America/Edmonton", &format!("{d}T090000")))
            .collect();
        assert_eq!(starts(&v), want);
        assert_eq!(v[0].end, loc("America/Edmonton", "20260907T100000"));
        assert!(v.iter().all(|e| e.title == "Standup" && !e.all_day));
    }

    #[test]
    fn monthly_last_friday() {
        let v = run("BEGIN:VEVENT
UID:m1
DTSTART:20260130T170000
DTEND:20260130T180000
RRULE:FREQ=MONTHLY;BYDAY=-1FR
SUMMARY:Payday
END:VEVENT");
        assert_eq!(starts(&v), vec!["2026-08-28T17:00:00", "2026-09-25T17:00:00"]);
        assert_eq!(v[1].end, "2026-09-25T18:00:00");
    }

    fn rule_dates(rule: &str, start: &str, n: usize) -> Vec<String> {
        let r = parse_rrule(rule).unwrap();
        let s = NaiveDateTime::parse_from_str(start, "%Y%m%dT%H%M%S").unwrap();
        let limit = NaiveDate::from_ymd_opt(2100, 1, 1).unwrap().and_time(NaiveTime::MIN);
        let until = r.until.map(|u| match u {
            IcsTime::Date(d) => d.and_hms_opt(23, 59, 59).unwrap(),
            IcsTime::Time(t, _) => t,
        });
        expand_rrule(&r, s, until, limit).into_iter().take(n).map(|d| d.format("%Y-%m-%d").to_string()).collect()
    }

    #[test]
    fn rrule_rfc_examples() {
        // WKST matters with INTERVAL=2 (RFC 5545 example).
        assert_eq!(
            rule_dates("FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=MO", "19970805T090000", 10),
            vec!["1997-08-05", "1997-08-10", "1997-08-19", "1997-08-24"]
        );
        assert_eq!(
            rule_dates("FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=SU", "19970805T090000", 10),
            vec!["1997-08-05", "1997-08-17", "1997-08-19", "1997-08-31"]
        );
        // Last weekday of the month.
        assert_eq!(
            rule_dates("FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1", "20260130T090000", 3),
            vec!["2026-01-30", "2026-02-27", "2026-03-31"]
        );
        // Second Monday, every other month.
        assert_eq!(
            rule_dates("FREQ=MONTHLY;INTERVAL=2;BYDAY=2MO", "20260112T090000", 3),
            vec!["2026-01-12", "2026-03-09", "2026-05-11"]
        );
        // US Thanksgiving.
        assert_eq!(
            rule_dates("FREQ=YEARLY;BYMONTH=11;BYDAY=4TH", "20251127T120000", 2),
            vec!["2025-11-27", "2026-11-26"]
        );
        // The 31st only in months that have one.
        assert_eq!(rule_dates("FREQ=MONTHLY", "20260131T090000", 3), vec!["2026-01-31", "2026-03-31", "2026-05-31"]);
        // Daily with UNTIL (date) and COUNT.
        assert_eq!(rule_dates("FREQ=DAILY;UNTIL=20260103", "20260101T090000", 10).len(), 3);
        assert_eq!(rule_dates("FREQ=DAILY;INTERVAL=3;COUNT=2", "20260101T090000", 10), vec!["2026-01-01", "2026-01-04"]);
        // Yearly on the 1st and 15th of March.
        assert_eq!(
            rule_dates("FREQ=YEARLY;BYMONTH=3;BYMONTHDAY=1,15", "20260301T090000", 3),
            vec!["2026-03-01", "2026-03-15", "2027-03-01"]
        );
        // Friday the 13th.
        assert_eq!(rule_dates("FREQ=MONTHLY;BYDAY=FR;BYMONTHDAY=13", "20260213T090000", 2), vec!["2026-02-13", "2026-03-13"]);
        // 100th day of the year, and the 20th Monday of the year.
        assert_eq!(rule_dates("FREQ=YEARLY;BYYEARDAY=100", "20260410T090000", 1), vec!["2026-04-10"]);
        assert_eq!(rule_dates("FREQ=YEARLY;BYDAY=20MO", "19970519T090000", 2), vec!["1997-05-19", "1998-05-18"]);
        // Monday of ISO week 20.
        assert_eq!(rule_dates("FREQ=YEARLY;BYWEEKNO=20;BYDAY=MO", "19970512T090000", 2), vec!["1997-05-12", "1998-05-11"]);
    }

    #[test]
    fn all_day_multi_and_single_day() {
        let v = run("BEGIN:VEVENT
UID:a1
DTSTART;VALUE=DATE:20260920
DTEND;VALUE=DATE:20260923
SUMMARY:Trip
END:VEVENT
BEGIN:VEVENT
UID:a2
DTSTART;VALUE=DATE:20260921
DTEND;VALUE=DATE:20260922
SUMMARY:Holiday
END:VEVENT
BEGIN:VEVENT
UID:a3
DTSTART;VALUE=DATE:20260922
SUMMARY:No end
END:VEVENT
BEGIN:VEVENT
UID:a4
DTSTART;VALUE=DATE:20260823
DTEND;VALUE=DATE:20260826
SUMMARY:Starts before window
END:VEVENT
BEGIN:VEVENT
UID:a5
DTSTART;VALUE=DATE:20260822
DTEND;VALUE=DATE:20260825
SUMMARY:Ends before window
END:VEVENT");
        let got: Vec<(String, String, String)> = v.iter().map(|e| (e.title.clone(), e.start.clone(), e.end.clone())).collect();
        assert!(v.iter().all(|e| e.all_day));
        assert!(got.contains(&("Trip".into(), "2026-09-20".into(), "2026-09-22".into())));
        assert!(got.contains(&("Holiday".into(), "2026-09-21".into(), "2026-09-21".into())));
        assert!(got.contains(&("No end".into(), "2026-09-22".into(), "2026-09-22".into())));
        assert!(got.contains(&("Starts before window".into(), "2026-08-23".into(), "2026-08-25".into())));
        assert_eq!(v.len(), 4);
    }

    #[test]
    fn yearly_all_day_birthday() {
        let v = run("BEGIN:VEVENT
UID:b1
DTSTART;VALUE=DATE:19900915
DTEND;VALUE=DATE:19900916
RRULE:FREQ=YEARLY
SUMMARY:Birthday
END:VEVENT");
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].start.as_str(), v[0].end.as_str(), v[0].all_day), ("2026-09-15", "2026-09-15", true));
    }

    #[test]
    fn recurrence_id_override_and_cancel() {
        let v = run("BEGIN:VEVENT
UID:r1
DTSTART:20260914T080000
DTEND:20260914T083000
RRULE:FREQ=DAILY;COUNT=5
SUMMARY:Walk
END:VEVENT
BEGIN:VEVENT
UID:r1
RECURRENCE-ID:20260916T080000
DTSTART:20260916T113000
DTEND:20260916T120000
SUMMARY:Walk (moved)
END:VEVENT
BEGIN:VEVENT
UID:r1
RECURRENCE-ID:20260917T080000
DTSTART:20260917T080000
DTEND:20260917T083000
STATUS:CANCELLED
SUMMARY:Walk
END:VEVENT
BEGIN:VEVENT
UID:c1
DTSTART:20260918T080000
STATUS:CANCELLED
SUMMARY:Gone
END:VEVENT");
        let got: Vec<(String, String)> = v.iter().map(|e| (e.start.clone(), e.title.clone())).collect();
        assert_eq!(
            got,
            vec![
                ("2026-09-14T08:00:00".to_string(), "Walk".to_string()),
                ("2026-09-15T08:00:00".to_string(), "Walk".to_string()),
                ("2026-09-16T11:30:00".to_string(), "Walk (moved)".to_string()),
                ("2026-09-18T08:00:00".to_string(), "Walk".to_string()),
            ]
        );
    }

    #[test]
    fn utc_and_tzid_conversion() {
        let v = run("BEGIN:VEVENT
UID:u1
DTSTART:20260915T160000Z
DTEND:20260915T170000Z
SUMMARY:UTC call
END:VEVENT
BEGIN:VEVENT
UID:t1
DTSTART;TZID=America/New_York:20260916T090000
DTEND;TZID=America/New_York:20260916T093000
SUMMARY:NY
END:VEVENT
BEGIN:VEVENT
UID:t2
DTSTART;TZID=Eastern Standard Time:20260917T090000
DURATION:PT45M
SUMMARY:Windows zone
END:VEVENT
BEGIN:VEVENT
UID:t3
DTSTART;TZID=\"/mozilla.org/20050126_1/Asia/Tokyo\":20260918T090000
SUMMARY:Prefixed
END:VEVENT
BEGIN:VEVENT
UID:f1
DTSTART:20260919T090000
SUMMARY:Floating
END:VEVENT");
        let by = |t: &str| v.iter().find(|e| e.title == t).unwrap().clone();
        assert_eq!(by("UTC call").start, utc_loc("20260915T160000"));
        assert_eq!(by("UTC call").end, utc_loc("20260915T170000"));
        assert_eq!(by("NY").start, loc("America/New_York", "20260916T090000"));
        assert_eq!(by("NY").end, loc("America/New_York", "20260916T093000"));
        assert_eq!(by("Windows zone").start, loc("America/New_York", "20260917T090000"));
        assert_eq!(by("Windows zone").end, loc("America/New_York", "20260917T094500"));
        assert_eq!(by("Prefixed").start, loc("Asia/Tokyo", "20260918T090000"));
        assert_eq!(by("Prefixed").end, by("Prefixed").start);
        assert_eq!(by("Floating").start, "2026-09-19T09:00:00");
    }

    #[test]
    fn windows_zone_names() {
        for (w, iana) in [
            ("Pacific Standard Time", "America/Los_Angeles"),
            ("Mountain Standard Time", "America/Denver"),
            ("Central Standard Time", "America/Chicago"),
            ("GMT Standard Time", "Europe/London"),
            ("W. Europe Standard Time", "Europe/Berlin"),
            ("AUS Eastern Standard Time", "Australia/Sydney"),
            ("India Standard Time", "Asia/Kolkata"),
            ("China Standard Time", "Asia/Shanghai"),
            ("Tokyo Standard Time", "Asia/Tokyo"),
        ] {
            assert_eq!(resolve_tzid(w), Zone::Named(iana.parse().unwrap()), "{w}");
        }
        assert!(matches!(resolve_tzid("UTC"), Zone::Named(_)));
        assert_eq!(resolve_tzid("Totally Made Up"), Zone::Floating);
        for (_, iana) in WINDOWS_ZONES {
            assert!(iana.parse::<chrono_tz::Tz>().is_ok(), "{iana}");
        }
    }

    #[test]
    fn folded_lines_params_and_escapes() {
        let text = "BEGIN:VCALENDAR\nBEGIN:VEVENT\r\nUID:f\r\nDTSTART;TZID=\"America/Edmonton\";X-A=\"a;b:c\":20260915T\r\n 100000\r\nSUMMARY:  Line one\\, with comma\\; semi\r\n\t and fold \\\\ done\\nnext  \r\nATTENDEE;CN=\"Doe, John: Jr\":mailto:x@example.com\r\nBEGIN:VALARM\r\nSUMMARY:alarm text\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:todo\r\nDTSTART:20260915T100000\r\nSUMMARY:A task\r\nEND:VTODO\r\nEND:VCALENDAR";
        let (ws, we) = visible_window(today());
        let v = events_from_ics(text, 3, ws, we).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].feed, 3);
        assert_eq!(v[0].title, "Line one, with comma; semi and fold \\ done\nnext");
        assert_eq!(v[0].start, loc("America/Edmonton", "20260915T100000"));
    }

    #[test]
    fn untitled_and_rdate() {
        let v = run("BEGIN:VEVENT
UID:x
DTSTART:20260901T100000
DTEND:20260901T110000
RDATE:20260910T100000,20260912T140000
SUMMARY:
END:VEVENT");
        assert_eq!(starts(&v), vec!["2026-09-01T10:00:00", "2026-09-10T10:00:00", "2026-09-12T14:00:00"]);
        assert!(v.iter().all(|e| e.title == "(untitled)"));
        assert_eq!(v[2].end, "2026-09-12T15:00:00");
    }

    #[test]
    fn not_ics() {
        assert!(events_from_ics("<html>nope</html>", 0, today(), today()).is_err());
    }

    #[test]
    fn cache_name_format() {
        let p = cache_path_for(Path::new("c"), "  https://example.com/a.ics ");
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(name, cache_path_for(Path::new("c"), "https://example.com/a.ics").file_name().unwrap().to_str().unwrap());
        assert!(name.starts_with("feed_") && name.ends_with(".ics") && name.len() == 5 + 16 + 4);
        assert!(name[5..21].chars().all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()));
    }

    #[test]
    fn shorten_like_csharp() {
        let long = "x".repeat(130);
        let s = shorten(&long);
        assert_eq!(s.chars().count(), 121);
        assert!(s.ends_with('\u{2026}'));
        assert_eq!(shorten("short"), "short");
    }

    #[test]
    fn google_share_link() {
        assert!(is_google_share_link("https://calendar.google.com/calendar/u/0?cid=abc"));
        assert!(!is_google_share_link("https://calendar.google.com/calendar/ical/x/private-y/basic.ics"));
    }

    #[tokio::test]
    async fn fetch_all_disabled_and_local_file() {
        let dir = std::env::temp_dir().join(format!("cal_feeds_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ics_path = dir.join("local.ics");
        std::fs::write(&ics_path, cal("BEGIN:VEVENT\nUID:l\nDTSTART;VALUE=DATE:20260910\nSUMMARY:Local\nEND:VEVENT")).unwrap();
        let feeds = vec![
            FeedCfg { url: "https://x.invalid/a.ics".into(), name: " ".into(), color: "#111".into(), enabled: false },
            FeedCfg { url: format!("file:///{}", ics_path.display().to_string().replace('\\', "/")), name: "".into(), color: "#222".into(), enabled: true },
            FeedCfg { url: ics_path.display().to_string(), name: " Mine ".into(), color: "#333".into(), enabled: true },
        ];
        let r = fetch_all(&feeds, &dir.join("cache"), false, today()).await;
        assert_eq!(r.statuses.len(), 3);
        assert_eq!(r.statuses[0].name, "Feed 1");
        assert!(!r.statuses[0].enabled);
        assert_eq!(r.statuses[1].name, "Feed 2");
        assert_eq!(r.statuses[1].error, None);
        assert_eq!(r.statuses[2].name, "Mine");
        assert_eq!(r.events.len(), 2);
        assert!(r.events.iter().any(|e| e.feed == 1) && r.events.iter().any(|e| e.feed == 2));
        assert!(cache_path_for(&dir.join("cache"), &feeds[2].url).exists());
        assert_eq!(test_feed(&feeds[2].url).await, Ok("1 events".to_string()));

        // Cache fallback when the source disappears: stale, with an error.
        std::fs::remove_file(&ics_path).unwrap();
        let r = fetch_all(&feeds[2..], &dir.join("cache"), false, today()).await;
        assert!(r.statuses[0].stale);
        assert!(r.statuses[0].error.is_some());
        assert_eq!(r.events.len(), 1);
        let r = fetch_all(&feeds[2..], &dir.join("cache"), true, today()).await;
        assert!(!r.statuses[0].stale && r.statuses[0].error.is_none() && !r.network);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
