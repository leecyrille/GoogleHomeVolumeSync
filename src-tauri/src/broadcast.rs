//! Broadcast a spoken message to Google speakers and displays.
//!
//! Pause what's playing, set the broadcast volume, play the message (a Windows
//! voice, after a short chime), then put every volume back and resume. Media
//! this app cast is loaded again at the same spot; other apps (Spotify, ...)
//! are closed by the message on those speakers, so the caller is told to press
//! play there.

use crate::core::Core;
use crate::types::{Backend, CastItem, DeviceCmd};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

static BUSY: AtomicBool = AtomicBool::new(false);

#[derive(Deserialize)]
pub struct Request {
    /// Google speakers, displays or speaker groups.
    pub targets: Vec<String>,
    pub text: String,
    pub chime: bool,
    /// 0..1
    pub volume: f32,
    pub voice: Option<String>,
}

#[derive(Serialize)]
pub struct Outcome {
    /// Speakers where music has to be started again by hand (e.g. Spotify).
    pub resume_by_hand: Vec<String>,
}

/// Something that was playing before the message.
struct Paused {
    owner: String,
    owner_name: String,
    app: Option<String>,
    content_id: Option<String>,
    content_type: Option<String>,
    title: Option<String>,
    position_ms: u64,
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// The voices Windows can speak with.
pub fn voices() -> Vec<String> {
    use windows::Media::SpeechSynthesis::SpeechSynthesizer;
    let list = || -> windows::core::Result<Vec<String>> {
        let mut out = Vec::new();
        for v in SpeechSynthesizer::AllVoices()? {
            out.push(v.DisplayName()?.to_string());
        }
        Ok(out)
    };
    list().unwrap_or_default()
}

/// Speak `text` into WAV bytes with a Windows voice.
fn speak(text: &str, voice: Option<&str>) -> Result<Vec<u8>, String> {
    use windows::core::HSTRING;
    use windows::Media::SpeechSynthesis::SpeechSynthesizer;
    use windows::Storage::Streams::DataReader;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let run = || -> windows::core::Result<Vec<u8>> {
        let synth = SpeechSynthesizer::new()?;
        if let Some(name) = voice {
            for v in SpeechSynthesizer::AllVoices()? {
                if v.DisplayName()?.to_string() == name {
                    synth.SetVoice(&v)?;
                }
            }
        }
        let stream = synth.SynthesizeTextToStreamAsync(&HSTRING::from(text))?.get()?;
        let size = stream.Size()? as u32;
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
        reader.LoadAsync(size)?.get()?;
        let mut buf = vec![0u8; size as usize];
        reader.ReadBytes(&mut buf)?;
        Ok(buf)
    };
    run().map_err(|e| format!("Windows couldn't speak the message: {e}"))
}

/// Put a two-tone chime and a little silence in front of 16-bit PCM WAV speech.
/// Returns the new WAV and its length in seconds.
fn with_chime(wav: &[u8], chime: bool) -> Result<(Vec<u8>, f64), String> {
    let bad = || "The spoken message came out in an unexpected format.".to_string();
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err(bad());
    }
    let (mut channels, mut rate, mut bits) = (0u16, 0u32, 0u16);
    let mut data: Option<&[u8]> = None;
    let mut i = 12;
    while i + 8 <= wav.len() {
        let id = &wav[i..i + 4];
        let len = u32::from_le_bytes(wav[i + 4..i + 8].try_into().unwrap()) as usize;
        let body = &wav[i + 8..(i + 8 + len).min(wav.len())];
        if id == b"fmt " && body.len() >= 16 {
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            data = Some(body);
        }
        i += 8 + len + (len & 1);
    }
    let data = data.ok_or_else(bad)?;
    if bits != 16 || channels == 0 || rate == 0 {
        return Err(bad());
    }
    let mut pcm: Vec<i16> = Vec::new();
    let silence = |pcm: &mut Vec<i16>, secs: f64| pcm.extend(std::iter::repeat(0).take((rate as f64 * secs) as usize * channels as usize));
    silence(&mut pcm, 0.35); // some speakers clip the very start
    if chime {
        // "Ding-dong": two soft bell tones.
        for (freq, secs) in [(880.0f64, 0.45f64), (659.25, 0.7)] {
            let n = (rate as f64 * secs) as usize;
            for k in 0..n {
                let t = k as f64 / rate as f64;
                let env = (-t * 5.0).exp() * (t * 200.0).min(1.0);
                let v = ((2.0 * std::f64::consts::PI * freq * t).sin() * 0.8 + (2.0 * std::f64::consts::PI * freq * 2.0 * t).sin() * 0.2) * env * 0.33;
                let s = (v * i16::MAX as f64) as i16;
                for _ in 0..channels {
                    pcm.push(s);
                }
            }
        }
        silence(&mut pcm, 0.25);
    }
    for c in data.chunks_exact(2) {
        pcm.push(i16::from_le_bytes([c[0], c[1]]));
    }
    silence(&mut pcm, 0.4);
    let bytes = pcm.len() * 2;
    let mut out = Vec::with_capacity(44 + bytes);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + bytes as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(bytes as u32).to_le_bytes());
    for s in &pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    let secs = pcm.len() as f64 / channels as f64 / rate as f64;
    Ok((out, secs))
}

fn dir() -> PathBuf {
    let d = crate::config::config_dir().join("broadcast");
    let _ = std::fs::create_dir_all(&d);
    // Old messages (the file server keeps links a while; an hour is plenty).
    if let Ok(entries) = std::fs::read_dir(&d) {
        for e in entries.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|a| a > Duration::from_secs(3600)).unwrap_or(false);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    d
}

pub async fn broadcast(core: &Arc<Core>, req: Request) -> Result<Outcome, String> {
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return Err("Type a message first.".into());
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        return Err("A message is already playing.".into());
    }
    let result = run(core, req, text).await;
    BUSY.store(false, Ordering::SeqCst);
    result
}

async fn run(core: &Arc<Core>, req: Request, text: String) -> Result<Outcome, String> {
    // 1. The message itself, before anything is paused.
    let voice = req.voice.clone();
    let chime = req.chime;
    let (wav, secs) = tokio::task::spawn_blocking(move || speak(&text, voice.as_deref()).and_then(|w| with_chime(&w, chime)))
        .await.map_err(|e| e.to_string())??;
    let path = dir().join(format!("message-{}.wav", uuid::Uuid::new_v4().simple()));
    std::fs::write(&path, &wav).map_err(|e| e.to_string())?;

    // 2. Where it plays, which speakers it affects, and what's playing on them.
    let (cast_targets, speakers, paused, saved) = {
        let inner = core.inner.lock().unwrap();
        let devs = &inner.devices;
        let is_cast = |id: &String| devs.get(id).map(|e| e.info.backend == Backend::Cast && e.info.online).unwrap_or(false);
        let targets: Vec<String> = req.targets.iter().filter(|id| is_cast(id)).cloned().collect();
        let groups: Vec<&String> = targets.iter().filter(|id| devs[*id].info.is_cast_group).collect();
        let mut speakers: HashSet<String> = HashSet::new();
        for t in &targets {
            let e = &devs[t];
            if e.info.is_cast_group {
                speakers.extend(e.info.members.iter().filter(|m| devs.contains_key(*m)).cloned());
            } else {
                speakers.insert(t.clone());
            }
        }
        // A speaker inside a targeted group hears it through the group.
        let cast_targets: Vec<String> = targets.iter()
            .filter(|t| devs[*t].info.is_cast_group || !groups.iter().any(|g| devs[*g].info.members.contains(t)))
            .cloned().collect();
        // Sessions to pause: on the speakers themselves or on any group they're in.
        let mut owners: Vec<String> = Vec::new();
        for (id, e) in devs.iter() {
            let covers = speakers.contains(id) || cast_targets.contains(id)
                || (e.info.is_cast_group && e.info.members.iter().any(|m| speakers.contains(m)));
            let playing = e.info.media.as_ref().map(|m| matches!(m.state.as_str(), "PLAYING" | "BUFFERING")).unwrap_or(false);
            if covers && playing && e.info.backend == Backend::Cast {
                owners.push(id.clone());
            }
        }
        let paused: Vec<Paused> = owners.iter().map(|id| {
            let e = &devs[id];
            let m = e.info.media.clone().unwrap_or_default();
            let pos = m.position_ms.map(|p| p as i64 + (unix_ms() - m.position_at.unwrap_or_else(unix_ms)).max(0)).unwrap_or(0).max(0) as u64;
            Paused {
                owner: id.clone(),
                owner_name: e.info.custom_name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| e.info.friendly_name.clone()),
                app: m.app.clone(),
                content_id: m.content_id.clone(),
                content_type: m.content_type.clone(),
                title: m.title.clone(),
                position_ms: pos,
            }
        }).collect();
        let saved: HashMap<String, (f32, bool)> = speakers.iter()
            .filter_map(|id| devs.get(id).map(|e| (id.clone(), (e.info.volume, e.info.muted)))).collect();
        (cast_targets, speakers, paused, saved)
    };
    if cast_targets.is_empty() {
        return Err("None of the chosen speakers are online.".into());
    }
    info!(targets=?cast_targets, speakers=speakers.len(), paused=paused.len(), secs, "broadcast: start");

    // 3. Pause, then the broadcast volume (sync groups hold still meanwhile).
    core.inner.lock().unwrap().hold_sync.extend(speakers.iter().cloned());
    for p in &paused {
        core.send_cmd(&p.owner, DeviceCmd::Pause);
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    let volume = req.volume.clamp(0.0, 1.0);
    for (id, (_, muted)) in &saved {
        core.set_device_volume(id, volume, true);
        if *muted {
            core.send_cmd(id, DeviceCmd::SetMuted(false));
        }
    }

    // 4. Play it everywhere at once and wait for it to finish.
    for id in &cast_targets {
        let ip = core.inner.lock().unwrap().devices.get(id).map(|e| e.info.ip.clone()).unwrap_or_default();
        match crate::media_server::share(&path, &ip).await {
            Ok(url) => core.send_cmd(id, DeviceCmd::Cast(vec![CastItem {
                url, title: "Message".into(), content_type: "audio/wav".into(), subtitles: None, autoplay: true,
            }])),
            Err(e) => warn!(id=%id, error=%e, "broadcast: couldn't share the message"),
        }
    }
    let started = Instant::now();
    tokio::time::sleep(Duration::from_secs_f64(secs + 3.5)).await;
    loop {
        let still = {
            let inner = core.inner.lock().unwrap();
            cast_targets.iter().any(|id| inner.devices.get(id).and_then(|e| e.info.media.as_ref())
                .map(|m| m.title.as_deref() == Some("Message") && matches!(m.state.as_str(), "PLAYING" | "BUFFERING")).unwrap_or(false))
        };
        if !still || started.elapsed() > Duration::from_secs_f64(secs + 20.0) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    for id in &cast_targets {
        core.send_cmd(id, DeviceCmd::StopCasting);
    }

    // 5. Volumes back, then the music.
    for (id, (vol, muted)) in &saved {
        core.set_device_volume(id, *vol, true);
        if *muted {
            core.send_cmd(id, DeviceCmd::SetMuted(true));
        }
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    let mut by_hand = Vec::new();
    for p in &paused {
        let interrupted = cast_targets.contains(&p.owner) || speakers.contains(&p.owner);
        let ours = p.app.as_deref() == Some("Default Media Receiver") && p.content_id.as_deref().map(|c| c.starts_with("http")).unwrap_or(false);
        if !interrupted {
            // Its app is still there (e.g. a group session around the speaker): just carry on.
            core.send_cmd(&p.owner, DeviceCmd::Play);
        } else if ours {
            core.send_cmd(&p.owner, DeviceCmd::Cast(vec![CastItem {
                url: p.content_id.clone().unwrap(),
                title: p.title.clone().unwrap_or_else(|| "Media".into()),
                content_type: p.content_type.clone().unwrap_or_else(|| "video/mp4".into()),
                subtitles: None,
                autoplay: true,
            }]));
            if p.position_ms > 3000 {
                let (core, owner, pos) = (core.clone(), p.owner.clone(), p.position_ms);
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    core.send_cmd(&owner, DeviceCmd::Seek(pos));
                });
            }
        } else {
            by_hand.push(format!("{}{}", p.owner_name, p.app.as_deref().map(|a| format!(" ({a})")).unwrap_or_default()));
        }
    }
    // Let the volume echoes arrive before sync groups listen again.
    let (c2, held) = (core.clone(), speakers.clone());
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(6)).await;
        let mut inner = c2.inner.lock().unwrap();
        for id in &held {
            inner.hold_sync.remove(id);
        }
    });
    info!(resume_by_hand=?by_hand, "broadcast: done");
    Ok(Outcome { resume_by_hand: by_hand })
}

#[cfg(test)]
mod tests {
    #[test]
    fn speaks_with_chime() {
        let wav = super::speak("Dinner is ready.", None).expect("speech");
        let (out, secs) = super::with_chime(&wav, true).expect("chime");
        assert_eq!(&out[0..4], b"RIFF");
        assert!(secs > 1.5 && secs < 10.0, "length {secs}");
        let path = std::env::temp_dir().join("ghvs-broadcast-test.wav");
        std::fs::write(&path, &out).unwrap();
        println!("wrote {} ({secs:.2} s, {} bytes); voices: {:?}", path.display(), out.len(), super::voices());
    }
}
