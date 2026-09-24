//! Turn one still picture (BGRA) into a short H.264 MP4 using Windows Media Foundation.
//! Nothing external is needed: Windows ships the H.264 encoder and the MP4 writer.

use std::path::{Path, PathBuf};

use windows::core::{Interface, GUID, HSTRING};
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Media Foundation time unit: 100 ns ticks per second.
const TICKS: i64 = 10_000_000;

/// Encode `bgra` (top-down rows, 4 bytes per pixel, stride = width*4) as an H.264 MP4 at `out`,
/// shown for `seconds` as `seconds * fps` identical frames (Roku players stall on a one-frame
/// file). Only the first frame is really encoded; the rest are tiny "nothing changed" frames
/// written directly. Blocking; call from a worker thread.
pub fn encode_still_mp4_frames(
    bgra: &[u8],
    width: u32,
    height: u32,
    seconds: u32,
    fps: u32,
    out: &Path,
) -> Result<(), String> {
    encode(bgra, width, height, seconds, Some(fps), out, &Tuning::default()).map(|_| ())
}

/// Knobs used by the test harness; the public functions use the defaults.
#[derive(Clone, Debug)]
pub(crate) struct Tuning {
    pub allow_hardware: bool,
    pub rate: Rate,
    /// Target bitrate in bits per second (bitrate modes, and the fallback if an encoder rejects Quality mode).
    pub bitrate: u32,
    /// 0..=100, higher is slower but a little better.
    pub quality_vs_speed: u32,
    /// Frames mode: write the repeat frames ourselves instead of encoding every frame.
    pub synth_repeats: bool,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            allow_hardware: true,
            rate: Rate::Quality(95),
            bitrate: 16_000_000,
            quality_vs_speed: 100,
            synth_repeats: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub(crate) enum Rate {
    PeakVbr,
    Cbr,
    /// 0..=100
    Quality(u32),
    /// Fixed quantiser, 0..=51 (lower is better).
    Qp(u32),
}

/// What the encode actually did (for logging / tests).
#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub(crate) struct EncodeInfo {
    pub encoder: String,
    pub hardware: bool,
    pub level: u32,
    pub used_encoder_settings: bool,
    /// Frames mode only: how the repeat frames were made.
    pub repeats: String,
}

/// How many frames to feed the encoder and over what time span.
struct Plan {
    rate: u32,
    frames: u32,
    total_ticks: i64,
    cavlc: bool,
}

pub(crate) fn encode(
    bgra: &[u8],
    width: u32,
    height: u32,
    seconds: u32,
    fps: Option<u32>,
    out: &Path,
    tuning: &Tuning,
) -> Result<EncodeInfo, String> {
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
    if let Some(f) = fps {
        if f == 0 || f > 30 {
            return Err(format!("fps must be 1..=30, got {f}"));
        }
    }

    let nv12 = bgra_to_nv12(&bgra[..need], width as usize, height as usize);
    let tmp = temp_path(out, "tmp");
    let first = temp_path(out, "first.tmp");

    let result = {
        let _com = ComGuard::init()?;
        let _mf = MfGuard::start()?;
        // All COM objects live and die inside these calls, before MFShutdown / CoUninitialize.
        unsafe { encode_to(&nv12, width, height, seconds, fps, &tmp, &first, tuning) }
    }
    .and_then(|info| move_index_to_front(&tmp).map(|_| info));
    let _ = std::fs::remove_file(&first);

    match result {
        Ok(info) => {
            // Replaces the old file even if a web server has it open (it opens with FILE_SHARE_DELETE).
            std::fs::rename(&tmp, out).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("rename {} -> {}: {e}", tmp.display(), out.display())
            })?;
            Ok(info)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn encode_to(
    nv12: &[u8],
    width: u32,
    height: u32,
    seconds: u32,
    fps: Option<u32>,
    tmp: &Path,
    first: &Path,
    tuning: &Tuning,
) -> Result<EncodeInfo, String> {
    let total_ticks = seconds as i64 * TICKS;
    let Some(fps) = fps else {
        // One frame lasting the whole time. The encoder is told 1 fps so the frame gets a full second of bits.
        let plan = Plan { rate: 1, frames: 1, total_ticks, cavlc: false };
        return write_mp4_any(nv12, width, height, &plan, tmp, tuning);
    };
    let frames = seconds * fps;
    let full = Plan { rate: fps, frames, total_ticks, cavlc: false };
    if !tuning.synth_repeats || frames < 2 {
        return write_mp4_any(nv12, width, height, &full, tmp, tuning).map(|mut i| {
            i.repeats = "encoded".into();
            i
        });
    }
    // Encode just the first frame (CAVLC, so repeat frames are simple to write), then copy it
    // into the final file followed by "skip everything" frames.
    // Encoded as if 1 fps (same bit budget as the one-frame file); the real rate is set when copying.
    let one = Plan { rate: 1, frames: 1, total_ticks: TICKS / fps as i64, cavlc: true };
    let mut info = write_mp4_any(nv12, width, height, &one, first, tuning)?;
    match remux_with_repeats(first, tmp, frames, fps, total_ticks) {
        Ok(()) => {
            info.repeats = "synthesized".into();
            Ok(info)
        }
        Err(why) => {
            // Unusual encoder output: fall back to encoding every frame.
            let mut info = write_mp4_any(nv12, width, height, &full, tmp, tuning)?;
            info.repeats = format!("encoded ({why})");
            Ok(info)
        }
    }
}

fn temp_path(out: &Path, suffix: &str) -> PathBuf {
    let name = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "video.mp4".into());
    out.with_file_name(format!("{name}.{}.{suffix}", std::process::id()))
}

struct ComGuard(bool);

impl ComGuard {
    fn init() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(ComGuard(true)) // S_OK or S_FALSE: must be balanced with CoUninitialize.
        } else if hr == RPC_E_CHANGED_MODE {
            Ok(ComGuard(false)) // Thread is already STA; Media Foundation still works.
        } else {
            Err(format!("CoInitializeEx failed: {hr:?}"))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

struct MfGuard;

impl MfGuard {
    fn start() -> Result<Self, String> {
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }.map_err(|e| format!("MFStartup failed: {e}"))?;
        Ok(MfGuard)
    }
}

impl Drop for MfGuard {
    fn drop(&mut self) {
        let _ = unsafe { MFShutdown() };
    }
}

// MFSetAttributeSize / MFSetAttributeRatio are inline helpers in mfapi.h: both pack two u32s into a u64.
unsafe fn set_pair(a: &IMFAttributes, key: &GUID, hi: u32, lo: u32) -> windows::core::Result<()> {
    a.SetUINT64(key, ((hi as u64) << 32) | lo as u64)
}

/// Smallest H.264 level whose frame-size limit fits (bitrate and rate limits are far above what we use).
fn h264_level(width: u32, height: u32) -> u32 {
    let mbs = width.div_ceil(16) * height.div_ceil(16);
    match mbs {
        0..=1620 => 31,
        1621..=8192 => 41,
        8193..=22080 => 50,
        _ => 51,
    }
}

fn err(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}

unsafe fn new_attributes(n: u32) -> Result<IMFAttributes, String> {
    let mut a: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut a, n).map_err(err("MFCreateAttributes"))?;
    a.ok_or_else(|| "MFCreateAttributes returned nothing".into())
}

unsafe fn new_sink_writer(path: &Path, allow_hardware: bool) -> Result<IMFSinkWriter, String> {
    let attrs = new_attributes(4)?;
    attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, allow_hardware as u32).map_err(err("attr"))?;
    attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1).map_err(err("attr"))?;
    // The temp file does not end in .mp4, so name the container explicitly.
    attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4).map_err(err("attr"))?;
    let url = HSTRING::from(path.as_os_str());
    MFCreateSinkWriterFromURL(&url, None::<&IMFByteStream>, &attrs).map_err(err("MFCreateSinkWriterFromURL"))
}

unsafe fn set_video_common(t: &IMFMediaType, width: u32, height: u32, fps: u32) -> windows::core::Result<()> {
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    set_pair(t, &MF_MT_FRAME_SIZE, width, height)?;
    set_pair(t, &MF_MT_FRAME_RATE, fps, 1)?;
    set_pair(t, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;
    t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
    t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
    t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
    t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
    Ok(())
}

/// `write_mp4`, retried with the software encoder if the hardware one fails
/// (for example when a consumer GPU has no free encoder sessions).
unsafe fn write_mp4_any(
    nv12: &[u8],
    width: u32,
    height: u32,
    plan: &Plan,
    path: &Path,
    tuning: &Tuning,
) -> Result<EncodeInfo, String> {
    match write_mp4(nv12, width, height, plan, path, tuning) {
        Err(e) if tuning.allow_hardware => {
            let sw = Tuning { allow_hardware: false, ..tuning.clone() };
            write_mp4(nv12, width, height, plan, path, &sw).map_err(|e2| format!("{e}; software retry: {e2}"))
        }
        r => r,
    }
}

unsafe fn write_mp4(
    nv12: &[u8],
    width: u32,
    height: u32,
    plan: &Plan,
    path: &Path,
    tuning: &Tuning,
) -> Result<EncodeInfo, String> {
    let writer = new_sink_writer(path, tuning.allow_hardware)?;
    let level = h264_level(width, height);

    let out_t = MFCreateMediaType().map_err(err("MFCreateMediaType"))?;
    set_video_common(&out_t, width, height, plan.rate).map_err(err("output type"))?;
    out_t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).map_err(err("output type"))?;
    out_t.SetUINT32(&MF_MT_AVG_BITRATE, tuning.bitrate).map_err(err("output type"))?;
    out_t.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32).map_err(err("output type"))?;
    out_t.SetUINT32(&MF_MT_MPEG2_LEVEL, level).map_err(err("output type"))?;
    let stream = writer.AddStream(&out_t).map_err(err("AddStream"))?;

    let in_t = MFCreateMediaType().map_err(err("MFCreateMediaType"))?;
    set_video_common(&in_t, width, height, plan.rate).map_err(err("input type"))?;
    in_t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).map_err(err("input type"))?;
    in_t.SetUINT32(&MF_MT_DEFAULT_STRIDE, width).map_err(err("input type"))?;
    in_t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1).map_err(err("input type"))?;

    // Try the full encoder settings, then without the rate-control choice, then plain defaults:
    // not every (hardware) encoder accepts every setting, and defaults still give a valid file.
    let mut used_encoder_settings = false;
    let mut last_err = None;
    for stage in 0..3 {
        let enc = if stage < 2 { Some(encoder_settings(tuning, plan, stage == 0)?) } else { None };
        match writer.SetInputMediaType(stream, &in_t, enc.as_ref()) {
            Ok(()) => {
                used_encoder_settings = stage == 0;
                last_err = None;
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = last_err {
        return Err(format!("SetInputMediaType: {e}"));
    }

    let (encoder, hardware) = describe_encoder(&writer, stream);

    let buffer = MFCreateMemoryBuffer(nv12.len() as u32).map_err(err("MFCreateMemoryBuffer"))?;
    {
        let mut p: *mut u8 = std::ptr::null_mut();
        buffer.Lock(&mut p, None, None).map_err(err("buffer Lock"))?;
        std::ptr::copy_nonoverlapping(nv12.as_ptr(), p, nv12.len());
        buffer.Unlock().map_err(err("buffer Unlock"))?;
        buffer.SetCurrentLength(nv12.len() as u32).map_err(err("SetCurrentLength"))?;
    }

    writer.BeginWriting().map_err(err("BeginWriting"))?;
    // Every frame points at the same read-only buffer.
    let n = plan.frames.max(1) as i64;
    for i in 0..n {
        let start = i * plan.total_ticks / n;
        let end = (i + 1) * plan.total_ticks / n;
        let sample = MFCreateSample().map_err(err("MFCreateSample"))?;
        sample.AddBuffer(&buffer).map_err(err("AddBuffer"))?;
        sample.SetSampleTime(start).map_err(err("SetSampleTime"))?;
        sample.SetSampleDuration(end - start).map_err(err("SetSampleDuration"))?;
        writer.WriteSample(stream, &sample).map_err(err("WriteSample"))?;
    }
    writer.Finalize().map_err(err("Finalize"))?;

    Ok(EncodeInfo { encoder, hardware, level, used_encoder_settings, repeats: String::new() })
}

unsafe fn encoder_settings(tuning: &Tuning, plan: &Plan, with_rate: bool) -> Result<IMFAttributes, String> {
    let e = new_attributes(8)?;
    (|| -> windows::core::Result<()> {
        if with_rate {
            match tuning.rate {
                Rate::PeakVbr => {
                    e.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32)?;
                    e.SetUINT32(&CODECAPI_AVEncCommonMeanBitRate, tuning.bitrate)?;
                    e.SetUINT32(&CODECAPI_AVEncCommonMaxBitRate, tuning.bitrate.saturating_mul(2))?;
                }
                Rate::Cbr => {
                    e.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR.0 as u32)?;
                    e.SetUINT32(&CODECAPI_AVEncCommonMeanBitRate, tuning.bitrate)?;
                }
                Rate::Quality(q) => {
                    e.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_Quality.0 as u32)?;
                    e.SetUINT32(&CODECAPI_AVEncCommonQuality, q)?;
                }
                Rate::Qp(qp) => {
                    e.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_Quality.0 as u32)?;
                    e.SetUINT64(&CODECAPI_AVEncVideoEncodeQP, qp as u64)?;
                }
            }
        }
        // One keyframe at the start, no B-frames.
        e.SetUINT32(&CODECAPI_AVEncMPVGOPSize, plan.frames.max(1))?;
        e.SetUINT32(&CODECAPI_AVEncMPVDefaultBPictureCount, 0)?;
        e.SetUINT32(&CODECAPI_AVEncCommonQualityVsSpeed, tuning.quality_vs_speed)?;
        // CAVLC lets us write the repeat frames by hand. Otherwise leave the encoder's default:
        // forcing CABAC on the Microsoft software encoder gave a corrupt picture in testing.
        if plan.cavlc {
            e.SetUINT32(&CODECAPI_AVEncH264CABACEnable, 0)?;
        }
        Ok(())
    })()
    .map_err(err("encoder settings"))?;
    Ok(e)
}

/// Best-effort name of the encoder MFT the sink writer picked.
unsafe fn describe_encoder(writer: &IMFSinkWriter, stream: u32) -> (String, bool) {
    let mut raw: *mut core::ffi::c_void = std::ptr::null_mut();
    if writer.GetServiceForStream(stream, &GUID::zeroed(), &IMFTransform::IID, &mut raw).is_err() || raw.is_null() {
        return ("unknown".into(), false);
    }
    let mft = IMFTransform::from_raw(raw);
    let Ok(a) = mft.GetAttributes() else {
        return ("Microsoft software".into(), false);
    };
    let get = |key: &GUID| -> Option<String> {
        let len = a.GetStringLength(key).ok()?;
        let mut buf = vec![0u16; len as usize + 1];
        a.GetString(key, &mut buf, None).ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    };
    let hw_url = get(&MFT_ENUM_HARDWARE_URL_Attribute);
    let name = get(&MFT_FRIENDLY_NAME_Attribute);
    let hardware = hw_url.is_some();
    let label = match (name, hw_url) {
        (Some(n), _) => n,
        (None, Some(u)) => u,
        (None, None) => "Microsoft software".into(),
    };
    (label, hardware)
}

// ---------------------------------------------------------------------------------------------
// Repeat frames: read back the single encoded frame, then write it plus `frames - 1` P-frames in
// which every macroblock is "skipped" (copy of the previous frame). No re-encoding needed.

unsafe fn remux_with_repeats(first: &Path, out: &Path, frames: u32, fps: u32, total_ticks: i64) -> Result<(), String> {
    let url = HSTRING::from(first.as_os_str());
    let reader = MFCreateSourceReaderFromURL(&url, None::<&IMFAttributes>).map_err(err("read back"))?;
    let vs = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    let native = reader.GetNativeMediaType(vs, 0).map_err(err("native type"))?;

    let mut flags = 0u32;
    let mut sample: Option<IMFSample> = None;
    reader.ReadSample(vs, 0, None, Some(&mut flags), None, Some(&mut sample)).map_err(err("ReadSample"))?;
    let sample = sample.ok_or("no encoded frame")?;
    let idr = sample_bytes(&sample)?;

    // SPS/PPS are in the media type's sequence header and/or in the frame itself.
    let mut headers = Vec::new();
    if let Ok(n) = native.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) {
        headers.resize(n as usize, 0);
        native.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut headers, None).map_err(err("sequence header"))?;
    }
    let mut sps = None;
    let mut pps = None;
    for nal in split_annexb(&headers).into_iter().chain(split_annexb(&idr)) {
        match nal.first().map(|b| b & 0x1f) {
            Some(7) if sps.is_none() => sps = Some(parse_sps(&unescape(&nal[1..]))?),
            Some(8) if pps.is_none() => pps = Some(parse_pps(&unescape(&nal[1..]))?),
            _ => {}
        }
    }
    let (sps, pps) = (sps.ok_or("no SPS")?, pps.ok_or("no PPS")?);
    if pps.cabac {
        return Err("encoder ignored the CAVLC request".into());
    }

    let t = MFCreateMediaType().map_err(err("MFCreateMediaType"))?;
    native.CopyAllItems(&t).map_err(err("copy type"))?;
    set_pair(&t, &MF_MT_FRAME_RATE, fps, 1).map_err(err("frame rate"))?;
    let writer = new_sink_writer(out, false)?;
    let stream = writer.AddStream(&t).map_err(err("AddStream (copy)"))?;
    writer.SetInputMediaType(stream, &t, None::<&IMFAttributes>).map_err(err("SetInputMediaType (copy)"))?;
    writer.BeginWriting().map_err(err("BeginWriting (copy)"))?;

    let n = frames as i64;
    for i in 0..n {
        let start = i * total_ticks / n;
        let end = (i + 1) * total_ticks / n;
        let bytes = if i == 0 { idr.clone() } else { skip_frame(&sps, &pps, i as u32) };
        let s = bytes_sample(&bytes)?;
        s.SetSampleTime(start).map_err(err("SetSampleTime"))?;
        s.SetSampleDuration(end - start).map_err(err("SetSampleDuration"))?;
        s.SetUINT32(&MFSampleExtension_CleanPoint, (i == 0) as u32).map_err(err("CleanPoint"))?;
        writer.WriteSample(stream, &s).map_err(err("WriteSample (copy)"))?;
    }
    writer.Finalize().map_err(err("Finalize (copy)"))
}

unsafe fn sample_bytes(s: &IMFSample) -> Result<Vec<u8>, String> {
    let buf = s.ConvertToContiguousBuffer().map_err(err("ConvertToContiguousBuffer"))?;
    let mut p: *mut u8 = std::ptr::null_mut();
    let mut len = 0u32;
    buf.Lock(&mut p, None, Some(&mut len)).map_err(err("Lock"))?;
    let v = std::slice::from_raw_parts(p, len as usize).to_vec();
    let _ = buf.Unlock();
    Ok(v)
}

unsafe fn bytes_sample(bytes: &[u8]) -> Result<IMFSample, String> {
    let buf = MFCreateMemoryBuffer(bytes.len() as u32).map_err(err("MFCreateMemoryBuffer"))?;
    let mut p: *mut u8 = std::ptr::null_mut();
    buf.Lock(&mut p, None, None).map_err(err("Lock"))?;
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
    buf.Unlock().map_err(err("Unlock"))?;
    buf.SetCurrentLength(bytes.len() as u32).map_err(err("SetCurrentLength"))?;
    let s = MFCreateSample().map_err(err("MFCreateSample"))?;
    s.AddBuffer(&buf).map_err(err("AddBuffer"))?;
    Ok(s)
}

/// Split an Annex-B byte stream (00 00 01 / 00 00 00 01 start codes) into NAL units.
fn split_annexb(d: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= d.len() {
        if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map(|&n| n - 3).unwrap_or(d.len());
        while e > s && d[e - 1] == 0 {
            e -= 1; // trailing zeros belong to the next start code
        }
        if e > s {
            nals.push(&d[s..e]);
        }
    }
    nals
}

/// Remove emulation-prevention bytes (00 00 03 -> 00 00).
fn unescape(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len());
    let mut zeros = 0;
    for &b in d {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// Insert emulation-prevention bytes.
fn escape(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len() + 16);
    let mut zeros = 0;
    for &b in d {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

struct BitReader<'a> {
    d: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn bit(&mut self) -> Result<u32, String> {
        let byte = *self.d.get(self.pos / 8).ok_or("parameter set too short")?;
        let b = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Ok(b as u32)
    }
    fn bits(&mut self, n: u32) -> Result<u32, String> {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Ok(v)
    }
    fn ue(&mut self) -> Result<u32, String> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err("bad exp-golomb code".into());
            }
        }
        Ok((1u32 << zeros) - 1 + self.bits(zeros)?)
    }
    fn se(&mut self) -> Result<i32, String> {
        let k = self.ue()?;
        Ok(if k % 2 == 1 { k.div_ceil(2) as i32 } else { -((k / 2) as i32) })
    }
}

#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    cur: u8,
    n: u8,
}

impl BitWriter {
    fn bit(&mut self, b: u32) {
        self.cur = (self.cur << 1) | (b & 1) as u8;
        self.n += 1;
        if self.n == 8 {
            self.out.push(self.cur);
            self.cur = 0;
            self.n = 0;
        }
    }
    fn bits(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.bit(v >> i);
        }
    }
    fn ue(&mut self, v: u32) {
        let x = v as u64 + 1;
        let len = 64 - x.leading_zeros();
        self.bits(0, len - 1);
        for i in (0..len).rev() {
            self.bit(((x >> i) & 1) as u32);
        }
    }
    /// rbsp_trailing_bits: a 1 then zeros to the byte boundary.
    fn finish(mut self) -> Vec<u8> {
        self.bit(1);
        while self.n != 0 {
            self.bit(0);
        }
        self.out
    }
}

struct Sps {
    log2_max_frame_num: u32,
    poc_type: u32,
    log2_max_poc_lsb: u32,
    mbs: u32,
}

struct Pps {
    id: u32,
    cabac: bool,
    bottom_field_poc: bool,
    deblock_control: bool,
    redundant_pic_cnt: bool,
}

fn skip_scaling_list(r: &mut BitReader, size: u32) -> Result<(), String> {
    let (mut last, mut next) = (8i32, 8i32);
    for _ in 0..size {
        if next != 0 {
            next = (last + r.se()? + 256) % 256;
        }
        last = if next == 0 { last } else { next };
    }
    Ok(())
}

fn parse_sps(d: &[u8]) -> Result<Sps, String> {
    let mut r = BitReader { d, pos: 0 };
    let profile = r.bits(8)?;
    r.bits(16)?; // constraint flags + level
    r.ue()?; // sps id
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        let chroma = r.ue()?;
        if chroma == 3 {
            r.bit()?;
        }
        r.ue()?;
        r.ue()?;
        r.bit()?;
        if r.bit()? == 1 {
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if r.bit()? == 1 {
                    skip_scaling_list(&mut r, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    let log2_max_frame_num = r.ue()? + 4;
    let poc_type = r.ue()?;
    let mut log2_max_poc_lsb = 0;
    match poc_type {
        0 => log2_max_poc_lsb = r.ue()? + 4,
        2 => {}
        _ => return Err("picture order count type 1 not handled".into()),
    }
    r.ue()?; // max_num_ref_frames
    r.bit()?; // gaps_in_frame_num_value_allowed_flag
    let w = r.ue()? + 1;
    let h = r.ue()? + 1;
    if r.bit()? != 1 {
        return Err("interlaced stream".into());
    }
    Ok(Sps { log2_max_frame_num, poc_type, log2_max_poc_lsb, mbs: w * h })
}

fn parse_pps(d: &[u8]) -> Result<Pps, String> {
    let mut r = BitReader { d, pos: 0 };
    let id = r.ue()?;
    r.ue()?; // sps id
    let cabac = r.bit()? == 1;
    let bottom_field_poc = r.bit()? == 1;
    if r.ue()? != 0 {
        return Err("slice groups not handled".into());
    }
    r.ue()?;
    r.ue()?;
    let weighted_pred = r.bit()? == 1;
    r.bits(2)?;
    r.se()?;
    r.se()?;
    r.se()?;
    let deblock_control = r.bit()? == 1;
    r.bit()?;
    let redundant_pic_cnt = r.bit()? == 1;
    if weighted_pred {
        return Err("weighted prediction not handled".into());
    }
    Ok(Pps { id, cabac, bottom_field_poc, deblock_control, redundant_pic_cnt })
}

/// One P-frame (reference picture, CAVLC) in which every macroblock is skipped, as Annex-B bytes.
fn skip_frame(sps: &Sps, pps: &Pps, index: u32) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.ue(0); // first_mb_in_slice
    w.ue(5); // slice_type: P (all slices)
    w.ue(pps.id);
    w.bits(index % (1 << sps.log2_max_frame_num), sps.log2_max_frame_num); // frame_num
    if sps.poc_type == 0 {
        w.bits((index * 2) % (1 << sps.log2_max_poc_lsb), sps.log2_max_poc_lsb);
        if pps.bottom_field_poc {
            w.ue(0); // delta_pic_order_cnt_bottom = 0 (se(0) == ue(0))
        }
    }
    if pps.redundant_pic_cnt {
        w.ue(0);
    }
    w.bit(1); // num_ref_idx_active_override_flag
    w.ue(0); // one reference picture
    w.bit(0); // ref_pic_list_modification_flag_l0
    w.bit(0); // adaptive_ref_pic_marking_mode_flag (sliding window)
    w.ue(0); // slice_qp_delta
    if pps.deblock_control {
        w.ue(1); // disable_deblocking_filter_idc: nothing to filter anyway
    }
    w.ue(sps.mbs); // mb_skip_run: the whole picture
    let rbsp = w.finish();
    let mut out = vec![0, 0, 0, 1, 0x41]; // nal_ref_idc 2, type 1 (non-IDR slice)
    out.extend(escape(&rbsp));
    out
}

// ---------------------------------------------------------------------------------------------

/// "Fast start": Media Foundation puts the index (moov) after the picture data (mdat).
/// Move it to the front so a player streaming over HTTP can start without seeking to the end.
/// (MF_MPEG4SINK_MOOV_BEFORE_MDAT exists but produced broken files in testing.)
fn move_index_to_front(path: &Path) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let boxes = top_level_boxes(&data)?;
    let find = |t: &[u8; 4]| boxes.iter().position(|b| &b.0 == t);
    let (Some(mi), Some(di)) = (find(b"moov"), find(b"mdat")) else {
        return Err("MP4 has no moov or mdat box".into());
    };
    if mi < di {
        return Ok(());
    }
    let (_, ms, me) = boxes[mi];
    let mut moov = data[ms..me].to_vec();
    let shift = moov.len() as u64;
    patch_chunk_offsets(&mut moov[8..], shift)?;

    // New order: everything before mdat, then moov, then the rest without the old moov.
    let insert_at = boxes[di].1;
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&data[..insert_at]);
    out.extend_from_slice(&moov);
    for (i, &(_, s, e)) in boxes.iter().enumerate() {
        if i >= di && i != mi {
            out.extend_from_slice(&data[s..e]);
        }
    }
    std::fs::write(path, out).map_err(|e| format!("write {}: {e}", path.display()))
}

fn top_level_boxes(d: &[u8]) -> Result<Vec<([u8; 4], usize, usize)>, String> {
    let mut v = Vec::new();
    let mut i = 0usize;
    while i + 8 <= d.len() {
        let mut size = u32::from_be_bytes(d[i..i + 4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = d[i + 4..i + 8].try_into().unwrap();
        if size == 1 && i + 16 <= d.len() {
            size = u64::from_be_bytes(d[i + 8..i + 16].try_into().unwrap());
        } else if size == 0 {
            size = (d.len() - i) as u64;
        }
        let end = i as u64 + size;
        if size < 8 || end > d.len() as u64 {
            return Err(format!("bad MP4 box at offset {i}"));
        }
        v.push((kind, i, end as usize));
        i = end as usize;
    }
    Ok(v)
}

/// Add `shift` to every stco/co64 entry inside the given box payload (recursing into containers).
fn patch_chunk_offsets(d: &mut [u8], shift: u64) -> Result<(), String> {
    let mut i = 0usize;
    while i + 8 <= d.len() {
        let size = u32::from_be_bytes(d[i..i + 4].try_into().unwrap()) as usize;
        if size < 8 || i + size > d.len() {
            return Err("bad box inside moov".into());
        }
        let kind: [u8; 4] = d[i + 4..i + 8].try_into().unwrap();
        let body = &mut d[i + 8..i + size];
        match &kind {
            b"trak" | b"mdia" | b"minf" | b"stbl" => patch_chunk_offsets(body, shift)?,
            b"stco" | b"co64" => {
                let wide = &kind == b"co64";
                let count = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
                let w = if wide { 8 } else { 4 };
                if 8 + count * w > body.len() {
                    return Err("bad chunk offset table".into());
                }
                for k in 0..count {
                    let e = &mut body[8 + k * w..8 + (k + 1) * w];
                    if wide {
                        let v = u64::from_be_bytes(e.try_into().unwrap()) + shift;
                        e.copy_from_slice(&v.to_be_bytes());
                    } else {
                        let v = u32::from_be_bytes(e.try_into().unwrap()) as u64 + shift;
                        let v = u32::try_from(v).map_err(|_| "file too large for stco".to_string())?;
                        e.copy_from_slice(&v.to_be_bytes());
                    }
                }
            }
            _ => {}
        }
        i += size;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------

/// BGRA (top-down) to NV12, BT.709 limited range. Chroma is the average of each 2x2 block.
pub(crate) fn bgra_to_nv12(bgra: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 3 / 2];
    let (y_plane, uv_plane) = out.split_at_mut(w * h);

    // Convert bands of row pairs on a few threads.
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8);
    let pairs_per_band = (h / 2).div_ceil(threads);
    std::thread::scope(|s| {
        let y_bands = y_plane.chunks_mut(pairs_per_band * 2 * w);
        let uv_bands = uv_plane.chunks_mut(pairs_per_band * w);
        let src_bands = bgra.chunks(pairs_per_band * 2 * w * 4);
        for ((yb, uvb), sb) in y_bands.zip(uv_bands).zip(src_bands) {
            s.spawn(move || convert_band(sb, yb, uvb, w));
        }
    });
    out
}

fn convert_band(src: &[u8], y: &mut [u8], uv: &mut [u8], w: usize) {
    let row = w * 4;
    for ((s2, y2), uvr) in src.chunks_exact(row * 2).zip(y.chunks_exact_mut(w * 2)).zip(uv.chunks_exact_mut(w)) {
        let (s0, s1) = s2.split_at(row);
        let (y0, y1) = y2.split_at_mut(w);
        for x in (0..w).step_by(2) {
            let (mut rs, mut gs, mut bs) = (0i32, 0i32, 0i32);
            for (src_row, y_row) in [(s0, &mut *y0), (s1, &mut *y1)] {
                for k in 0..2 {
                    let p = &src_row[(x + k) * 4..(x + k) * 4 + 3];
                    let (b, g, r) = (p[0] as i32, p[1] as i32, p[2] as i32);
                    y_row[x + k] = (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8;
                    rs += r;
                    gs += g;
                    bs += b;
                }
            }
            // Sums of 4 pixels; the extra >>2 averages them.
            let u = ((-26 * rs - 86 * gs + 112 * bs + 512) >> 10) + 128;
            let v = ((112 * rs - 102 * gs - 10 * bs + 512) >> 10) + 128;
            uvr[x] = u.clamp(16, 240) as u8;
            uvr[x + 1] = v.clamp(16, 240) as u8;
        }
    }
}
