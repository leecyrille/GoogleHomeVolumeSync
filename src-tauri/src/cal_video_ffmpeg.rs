//! Turn one still picture (BGRA) into a short H.264 MP4 with ffmpeg (Linux and other
//! non-Windows systems; Windows uses Media Foundation in cal_video.rs).
//! Needs the `ffmpeg` command with libx264 (Ubuntu: `sudo apt install ffmpeg`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Encode `bgra` (top-down rows, 4 bytes per pixel, stride = width*4) as an H.264 MP4 at `out`,
/// shown for `seconds` as `seconds * fps` identical frames (Roku players stall on a one-frame
/// file). Blocking; call from a worker thread.
pub fn encode_still_mp4_frames(
    bgra: &[u8],
    width: u32,
    height: u32,
    seconds: u32,
    fps: u32,
    out: &Path,
) -> Result<(), String> {
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return Err(format!("width and height must be even and non-zero, got {width}x{height}"));
    }
    if width > 4096 || height > 2304 {
        return Err(format!("{width}x{height} is larger than H.264 level 5.1 allows"));
    }
    let need = width as usize * height as usize * 4;
    if bgra.len() < need {
        return Err(format!("bgra is {} bytes, need {need}", bgra.len()));
    }
    if seconds == 0 || seconds > 24 * 3600 {
        return Err(format!("seconds must be 1..=86400, got {seconds}"));
    }
    if fps == 0 || fps > 30 {
        return Err(format!("fps must be 1..=30, got {fps}"));
    }

    let tmp = temp_path(out);
    let frames = seconds * fps;
    // One input frame, repeated to `frames` frames. x264's stillimage tuning makes the
    // repeats almost free; High profile, level 5.1 and yuv420p play on every Roku.
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgra"])
        .args(["-s", &format!("{width}x{height}"), "-framerate", &fps.to_string(), "-i", "-"])
        .args(["-vf", &format!("loop=loop={}:size=1:start=0,format=yuv420p", frames - 1)])
        .args(["-frames:v", &frames.to_string(), "-r", &fps.to_string()])
        .args(["-c:v", "libx264", "-preset", "medium", "-tune", "stillimage", "-crf", "16"])
        .args(["-profile:v", "high", "-level:v", "5.1", "-movflags", "+faststart", "-f", "mp4"])
        .arg(&tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't start ffmpeg (is it installed?): {e}"))?;
    let wrote = child.stdin.take().map(|mut i| i.write_all(&bgra[..need]));
    let result = child.wait_with_output().map_err(|e| format!("ffmpeg failed: {e}"));
    let result = result.and_then(|o| {
        if o.status.success() && matches!(wrote, Some(Ok(()))) {
            Ok(())
        } else {
            Err(format!("ffmpeg couldn't make the video: {}", String::from_utf8_lossy(&o.stderr).trim()))
        }
    });

    match result {
        // Replaces the old file even if the web server has it open.
        Ok(()) => std::fs::rename(&tmp, out).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("rename {} -> {}: {e}", tmp.display(), out.display())
        }),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn temp_path(out: &Path) -> PathBuf {
    let name = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "video.mp4".into());
    out.with_file_name(format!("{name}.{}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes_a_still() {
        let (w, h) = (320u32, 180u32);
        let bgra: Vec<u8> = (0..w * h).flat_map(|i| [(i % 256) as u8, 80, 160, 255]).collect();
        let out = std::env::temp_dir().join(format!("ghvs-still-{}.mp4", std::process::id()));
        super::encode_still_mp4_frames(&bgra, w, h, 5, 1, &out).expect("encode");
        let bytes = std::fs::read(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        assert_eq!(&bytes[4..8], b"ftyp");
        // faststart: the index (moov) comes before the picture data (mdat).
        let find = |tag: &[u8]| bytes.windows(4).position(|w| w == tag).unwrap();
        assert!(find(b"moov") < find(b"mdat"));
    }
}
