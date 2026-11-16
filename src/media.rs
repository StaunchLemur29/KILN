//! Opening pictures and videos, playing a video for the preview, and exporting.
//! Video goes through the ffmpeg / ffprobe programs.

use crate::fx::{FrameTime, Img, Renderer};
use crate::params::Params;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};

pub fn tool(name: &str) -> PathBuf {
    // the finished app carries its own copy beside the KILN program
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        let p = dir.join(name);
        if p.exists() {
            return p;
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        let p = Path::new(dir).join(name);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from(name)
}

pub fn ffmpeg_available() -> bool {
    Command::new(tool("ffmpeg")).arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp", "tga"];

pub fn is_image(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).map(|e| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false)
}

#[derive(Clone)]
pub struct VideoInfo {
    pub w: usize,
    pub h: usize,
    pub fps: f32,
    pub duration: f32,
    pub has_audio: bool,
}

pub fn probe(path: &Path) -> Result<VideoInfo, String> {
    let out = Command::new(tool("ffprobe"))
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"])
        .arg(path)
        .output()
        .map_err(|e| format!("Could not run ffprobe ({e}). Install ffmpeg to open videos."))?;
    if !out.status.success() {
        return Err("Could not open video. Try another file.".into());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let video = streams.iter().find(|s| s["codec_type"] == "video").ok_or("The file has no video track.")?;
    let mut w = video["width"].as_u64().unwrap_or(0) as usize;
    let mut h = video["height"].as_u64().unwrap_or(0) as usize;
    // phone videos store rotation as side data; ffmpeg auto-rotates, so swap the size
    let rot = video["side_data_list"]
        .as_array()
        .and_then(|l| l.iter().find_map(|d| d["rotation"].as_i64()))
        .or_else(|| video["tags"]["rotate"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    if rot.abs() % 180 == 90 {
        std::mem::swap(&mut w, &mut h);
    }
    let rate = |s: &str| -> f32 {
        let mut it = s.split('/');
        let a: f32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
        let b: f32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(1.0);
        if b > 0.0 { a / b } else { 0.0 }
    };
    let mut fps = rate(video["avg_frame_rate"].as_str().unwrap_or("0/1"));
    if !(1.0..=240.0).contains(&fps) {
        fps = rate(video["r_frame_rate"].as_str().unwrap_or("30/1"));
    }
    if !(1.0..=240.0).contains(&fps) {
        fps = 30.0;
    }
    let duration = v["format"]["duration"].as_str().and_then(|d| d.parse().ok()).or_else(|| video["duration"].as_str().and_then(|d| d.parse().ok())).unwrap_or(0.0);
    if w == 0 || h == 0 {
        return Err("Could not read the video size.".into());
    }
    Ok(VideoInfo { w, h, fps, duration, has_audio: streams.iter().any(|s| s["codec_type"] == "audio") })
}

/// Reads frames from a video at a given size, starting at `start` seconds, on its own thread.
pub struct Decoder {
    pub rx: Receiver<(f32, Img)>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Decoder {
    pub fn start(path: &Path, info: &VideoInfo, w: usize, h: usize, start: f32) -> Result<Decoder, String> {
        let mut child = Command::new(tool("ffmpeg"))
            .args(["-v", "error", "-nostdin", "-ss", &format!("{start:.3}")])
            .arg("-i")
            .arg(path)
            .args(["-an", "-vf", &format!("scale={w}:{h}:flags=area"), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Could not run ffmpeg ({e})."))?;
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx): (SyncSender<(f32, Img)>, _) = sync_channel(3);
        let fps = info.fps;
        std::thread::spawn(move || {
            let mut buf = vec![0u8; w * h * 3];
            let mut i = 0u32;
            while stdout.read_exact(&mut buf).is_ok() {
                let t = start + i as f32 / fps;
                if tx.send((t, Img::from_rgb8(w, h, &buf))).is_err() {
                    break;
                }
                i += 1;
            }
        });
        Ok(Decoder { rx, child: Arc::new(Mutex::new(Some(child))) })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

pub fn load_image(path: &Path) -> Result<Img, String> {
    let img = image::open(path).map_err(|_| "Could not open image. Choose another image or video.".to_string())?.to_rgb8();
    let (w, h) = img.dimensions();
    Ok(Img::from_rgb8(w as usize, h as usize, img.as_raw()))
}


#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    H264,
    Hevc,
    ProRes422,
    ProRes4444,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::H264, Format::Hevc, Format::ProRes422, Format::ProRes4444];
    pub fn key(self) -> &'static str {
        match self {
            Format::H264 => "H264",
            Format::Hevc => "HEVC",
            Format::ProRes422 => "PR422",
            Format::ProRes4444 => "PR4444",
        }
    }
    pub fn ext(self) -> &'static str {
        match self {
            Format::H264 | Format::Hevc => "mp4",
            _ => "mov",
        }
    }
}

/// Knob moves recorded while a clip plays: (seconds into the clip, settings from then on).
pub type Take = Vec<(f32, Params)>;

fn params_at<'a>(base: &'a Params, take: &'a Take, t: f32) -> &'a Params {
    take.iter().rev().find(|(at, _)| *at <= t).map(|(_, p)| p).or(take.first().map(|(_, p)| p)).unwrap_or(base)
}

pub enum Source {
    Image { img: Arc<Img>, path: PathBuf },
    Video { path: PathBuf, info: VideoInfo },
    /// A camera or stream; exports are recorded while it plays (see Recorder).
    Live { input: LiveInput },
}

pub struct ExportJob {
    pub progress: Arc<AtomicU32>, // 0..=1000
    pub cancel: Arc<AtomicBool>,
    pub result: Arc<Mutex<Option<Result<PathBuf, String>>>>,
}

impl ExportJob {
    pub fn fraction(&self) -> f32 {
        self.progress.load(Ordering::Relaxed) as f32 / 1000.0
    }
}

/// Export the processed picture or video. Nothing is drawn over the result.
pub fn start_export(source: &Source, params: Params, take: Take, format: Format, out: PathBuf, still_seconds: f32) -> ExportJob {
    let job = ExportJob {
        progress: Arc::new(AtomicU32::new(0)),
        cancel: Arc::new(AtomicBool::new(false)),
        result: Arc::new(Mutex::new(None)),
    };
    let (progress, cancel, result) = (job.progress.clone(), job.cancel.clone(), job.result.clone());
    let src = match source {
        Source::Image { img, .. } => SrcKind::Image(img.clone()),
        Source::Video { path, info } => SrcKind::Video(path.clone(), info.clone()),
        Source::Live { .. } => {
            *job.result.lock().unwrap() = Some(Err("Live sources are recorded with RECORD while they play.".into()));
            return job;
        }
    };
    std::thread::spawn(move || {
        let r = run_export(src, &params, &take, format, &out, still_seconds, &progress, &cancel);
        if r.is_err() || cancel.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&out);
        }
        let r = if cancel.load(Ordering::Relaxed) { Err("Export cancelled.".into()) } else { r.map(|_| out) };
        *result.lock().unwrap() = Some(r);
    });
    job
}

enum SrcKind {
    Image(Arc<Img>),
    Video(PathBuf, VideoInfo),
}

/// Output sizes must be even for H.264 / ProRes 4:2:2.
fn even(n: usize) -> usize {
    (n & !1).max(2)
}

fn run_export(src: SrcKind, params: &Params, take: &Take, format: Format, out: &Path, still_seconds: f32, progress: &AtomicU32, cancel: &AtomicBool) -> Result<(), String> {
    let mut renderer = Renderer::new();
    let tier = params.tier;
    match src {
        SrcKind::Image(img) => {
            let (sw, sh) = crate::fx::signal_size(tier, img.w, img.h, 4096);
            let small = img.resized(sw, sh);
            let (ow, oh) = (even(img.w), even(img.h));
            let fps = 30.0;
            let frames = (still_seconds * fps).round().max(1.0) as u32;
            let mut enc = Encoder::start(out, format, ow, oh, fps, None)?;
            for f in 0..frames {
                if cancel.load(Ordering::Relaxed) {
                    enc.abort();
                    return Err("Export cancelled.".into());
                }
                let ft = FrameTime { t: f as f32 / fps, frame: f, fps };
                let frame = renderer.render(&small, params_at(params, take, ft.t), &ft, ow, oh, 1.0);
                enc.write(&frame)?;
                progress.store(((f + 1) as f32 / frames as f32 * 990.0) as u32, Ordering::Relaxed);
            }
            enc.finish()?;
        }
        SrcKind::Video(path, info) => {
            let (sw, sh) = crate::fx::signal_size(tier, info.w, info.h, 4096);
            let (ow, oh) = (even(info.w), even(info.h));
            let dec = Decoder::start(&path, &info, sw, sh, 0.0)?;
            let audio = if info.has_audio { Some(path.as_path()) } else { None };
            let mut enc = Encoder::start(out, format, ow, oh, info.fps, audio)?;
            let total = (info.duration * info.fps).max(1.0);
            let mut n = 0u32;
            while let Ok((t, frame)) = dec.rx.recv() {
                if cancel.load(Ordering::Relaxed) {
                    enc.abort();
                    return Err("Export cancelled.".into());
                }
                let ft = FrameTime { t, frame: n, fps: info.fps };
                let img = renderer.render(&frame, params_at(params, take, t), &ft, ow, oh, 1.0);
                enc.write(&img)?;
                n += 1;
                progress.store(((n as f32 / total).min(1.0) * 990.0) as u32, Ordering::Relaxed);
            }
            if n == 0 {
                enc.abort();
                return Err("The source produced no video frames.".into());
            }
            enc.finish()?;
        }
    }
    progress.store(1000, Ordering::Relaxed);
    Ok(())
}

struct Encoder {
    child: Child,
    w: usize,
    h: usize,
}

impl Encoder {
    fn start(out: &Path, format: Format, w: usize, h: usize, fps: f32, audio: Option<&Path>) -> Result<Encoder, String> {
        let mut cmd = Command::new(tool("ffmpeg"));
        cmd.args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &format!("{w}x{h}"), "-r", &format!("{fps}"), "-i", "-"]);
        if let Some(a) = audio {
            cmd.arg("-i").arg(a).args(["-map", "0:v:0", "-map", "1:a:0?"]);
        }
        match format {
            Format::ProRes422 | Format::ProRes4444 => {
                if format == Format::ProRes422 {
                    cmd.args(["-c:v", "prores_ks", "-profile:v", "2", "-pix_fmt", "yuv422p10le"]);
                } else {
                    cmd.args(["-c:v", "prores_ks", "-profile:v", "4", "-pix_fmt", "yuv444p10le"]);
                }
                cmd.args(["-vendor", "apl0"]);
                if audio.is_some() {
                    cmd.args(["-c:a", "pcm_s16le"]);
                }
            }
            Format::H264 | Format::Hevc => {
                if format == Format::H264 {
                    cmd.args(["-c:v", "libx264", "-preset", "medium", "-crf", "16"]);
                } else {
                    cmd.args(["-c:v", "libx265", "-preset", "medium", "-crf", "18", "-tag:v", "hvc1", "-x265-params", "log-level=error"]);
                }
                cmd.args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"]);
                if audio.is_some() {
                    cmd.args(["-c:a", "aac", "-b:a", "256k"]);
                }
            }
        }
        if audio.is_some() {
            cmd.arg("-shortest");
        }
        let child = cmd.arg(out).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| format!("Could not start the video encoder ({e})."))?;
        Ok(Encoder { child, w, h })
    }

    fn write(&mut self, img: &Img) -> Result<(), String> {
        let img = if img.w != self.w || img.h != self.h { img.resized(self.w, self.h) } else { img.clone() };
        self.child.stdin.as_mut().unwrap().write_all(&img.to_rgb8()).map_err(|_| self.error("The encoder stopped before accepting a frame."))
    }

    fn error(&mut self, fallback: &str) -> String {
        let _ = self.child.kill();
        let mut msg = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = e.read_to_string(&mut msg);
        }
        let _ = self.child.wait();
        let msg = msg.trim();
        if msg.is_empty() { fallback.into() } else { format!("{fallback} {}", msg.lines().last().unwrap_or("")) }
    }

    fn finish(mut self) -> Result<(), String> {
        drop(self.child.stdin.take());
        let mut msg = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = e.read_to_string(&mut msg);
        }
        let st = self.child.wait().map_err(|e| e.to_string())?;
        if st.success() { Ok(()) } else { Err(format!("The video encoder did not finish. {}", msg.lines().last().unwrap_or(""))) }
    }

    fn abort(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}


// ------------------------------------------------------------------ live input

/// A live picture source: a camera or capture device on this Mac, or a network stream.
#[derive(Clone)]
pub enum LiveInput {
    Camera { index: String, name: String },
    Stream(String),
}

impl LiveInput {
    pub fn label(&self) -> String {
        match self {
            LiveInput::Camera { name, .. } => name.to_uppercase(),
            LiveInput::Stream(url) => url.clone(),
        }
    }
}

/// Video capture devices ffmpeg can open through AVFoundation: (index, name).
pub fn list_cameras() -> Vec<(String, String)> {
    let Ok(out) = Command::new(tool("ffmpeg")).args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""]).output() else {
        return vec![];
    };
    let text = String::from_utf8_lossy(&out.stderr);
    let mut in_video = false;
    let mut list = Vec::new();
    for line in text.lines() {
        if line.contains("AVFoundation video devices") {
            in_video = true;
            continue;
        }
        if line.contains("AVFoundation audio devices") {
            break;
        }
        if in_video {
            if let Some(rest) = line.split("] [").nth(1) {
                if let Some((idx, name)) = rest.split_once("] ") {
                    list.push((idx.trim().to_string(), name.trim().to_string()));
                }
            }
        }
    }
    list
}

/// Frames from a live source, scaled (letterboxed) to `w` x `h`, newest first.
pub struct LiveFeed {
    rx: Receiver<Img>,
    pending: Option<Img>,
    child: Arc<Mutex<Option<Child>>>,
}

impl LiveFeed {
    /// The newest picture since the last call, if any; Err when the source has gone away.
    pub fn latest(&mut self) -> Result<Option<Img>, ()> {
        let mut newest = self.pending.take();
        loop {
            match self.rx.try_recv() {
                Ok(img) => newest = Some(img),
                Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(newest),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return if newest.is_some() { Ok(newest) } else { Err(()) },
            }
        }
    }

    pub fn start(input: &LiveInput, w: usize, h: usize) -> Result<LiveFeed, LiveError> {
        let mut cmd = Command::new(tool("ffmpeg"));
        cmd.args(["-v", "error", "-nostdin"]);
        match input {
            LiveInput::Camera { index, name } => {
                cmd.args(["-f", "avfoundation"]);
                if name.to_lowercase().starts_with("capture screen") {
                    cmd.args(["-framerate", "30", "-capture_cursor", "1"]);
                } else {
                    // open the camera with a size and rate it really offers
                    let (size, fps) = camera_mode(index)?;
                    cmd.args(["-video_size", &size, "-framerate", &fps]);
                }
                cmd.args(["-i", &format!("{index}:none")]);
            }
            LiveInput::Stream(url) => {
                let realtime = ["rtsp://", "rtmp://", "udp://", "srt://", "rtp://", "tcp://"].iter().any(|p| url.starts_with(p));
                if realtime {
                    // a live network feed: show pictures as soon as they arrive
                    cmd.args(["-fflags", "nobuffer"]);
                } else {
                    // a file or web video: play it at its own speed, looping
                    cmd.args(["-re", "-stream_loop", "-1"]);
                }
                cmd.args(["-i", url]);
            }
        }
        let vf = format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2");
        let mut child = cmd
            .args(["-an", "-vf", &vf, "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| LiveError::plain(format!("Could not run ffmpeg ({e}).")))?;
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (tx, rx): (SyncSender<Img>, Receiver<Img>) = sync_channel(2);
        std::thread::spawn(move || {
            let mut buf = vec![0u8; w * h * 3];
            while stdout.read_exact(&mut buf).is_ok() {
                // drop frames the window has not caught up with: live means newest
                match tx.try_send(Img::from_rgb8(w, h, &buf)) {
                    Ok(_) | Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                    Err(_) => break,
                }
            }
        });
        let err_text = Arc::new(Mutex::new(String::new()));
        let et = err_text.clone();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            *et.lock().unwrap() = s;
        });
        // the first picture may wait on the system's camera permission question
        match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(first) => Ok(LiveFeed { rx, pending: Some(first), child: Arc::new(Mutex::new(Some(child))) }),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                std::thread::sleep(std::time::Duration::from_millis(150));
                let text = err_text.lock().unwrap().clone();
                Err(LiveError::from_ffmpeg(input, &text))
            }
        }
    }
}

/// Why a live source could not open, and which System Settings page can fix it.
pub struct LiveError {
    pub message: String,
    /// x-apple.systempreferences URL of the privacy page that grants access
    pub settings: Option<&'static str>,
}

const CAMERA_SETTINGS: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera";
const SCREEN_SETTINGS: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

impl LiveError {
    fn plain(message: String) -> LiveError {
        LiveError { message, settings: None }
    }

    fn from_ffmpeg(input: &LiveInput, text: &str) -> LiveError {
        let lower = text.to_lowercase();
        let screen = matches!(input, LiveInput::Camera { name, .. } if name.to_lowercase().starts_with("capture screen"));
        let denied = lower.contains("not authorized")
            || lower.contains("denied")
            || lower.contains("input/output error")
            || lower.contains("failed to create av capture input device");
        match input {
            LiveInput::Camera { .. } if denied && screen => LiveError {
                message: "macOS hasn't allowed KILN to record the screen. Turn KILN on under Privacy & Security → Screen & System Audio Recording, then try again.".into(),
                settings: Some(SCREEN_SETTINGS),
            },
            LiveInput::Camera { .. } if denied => LiveError {
                message: "macOS hasn't allowed KILN to use the camera. Turn KILN on under Privacy & Security → Camera, then try again.".into(),
                settings: Some(CAMERA_SETTINGS),
            },
            LiveInput::Camera { .. } => {
                LiveError::plain(format!("The camera sent no pictures. {}", text.trim().lines().last().unwrap_or("Is another app using it?")))
            }
            LiveInput::Stream(_) => LiveError::plain(format!(
                "The video feed sent no pictures. {}",
                text.trim().lines().last().unwrap_or("Check the address and that the feed is running.")
            )),
        }
    }
}

/// Ask a camera which sizes and frame rates it offers: asked for an impossible rate, ffmpeg's
/// AVFoundation input lists the supported modes ("1280x720@[1.000000 30.000030]fps"). Prefer
/// 1280 x 720, else the largest mode up to 1920 wide, at its highest exact rate (cameras often
/// report 30.000030, which a plain 30 does not match).
fn camera_mode(index: &str) -> Result<(String, String), LiveError> {
    let mut child = Command::new(tool("ffmpeg"))
        .args(["-hide_banner", "-nostdin", "-f", "avfoundation", "-framerate", "10000", "-i", &format!("{index}:none"), "-frames:v", "1", "-f", "null", "-"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| LiveError::plain(format!("Could not run ffmpeg ({e}).")))?;
    let mut stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    // the system's permission question can hold this up while the person answers it
    let text = match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(t) => t,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LiveError::plain("The camera did not answer. Is another app using it?".into()));
        }
    };
    let _ = child.wait();
    pick_mode(&text).ok_or_else(|| LiveError::from_ffmpeg(&LiveInput::Camera { index: index.into(), name: String::new() }, &text))
}

/// Choose a mode from ffmpeg's "Supported modes" list.
fn pick_mode(text: &str) -> Option<(String, String)> {
    let mut modes: Vec<(u32, u32, f64, String)> = Vec::new();
    for line in text.lines() {
        let Some(at) = line.find("@[") else { continue };
        let size = line[..at].split_whitespace().last().unwrap_or("");
        let Some((ws, hs)) = size.split_once('x') else { continue };
        let (Ok(mw), Ok(mh)) = (ws.parse::<u32>(), hs.parse::<u32>()) else { continue };
        let range = &line[at + 2..];
        let Some(end) = range.find(']') else { continue };
        let Some(max) = range[..end].split_whitespace().last() else { continue };
        let Ok(rate) = max.parse::<f64>() else { continue };
        modes.push((mw, mh, rate, max.to_string()));
    }
    if modes.is_empty() {
        return None;
    }
    let pick = modes
        .iter()
        .find(|m| m.0 == 1280 && m.1 == 720)
        .or_else(|| modes.iter().filter(|m| m.0 <= 1920).max_by(|a, b| (a.0 * a.1).cmp(&(b.0 * b.1)).then(a.2.total_cmp(&b.2))))
        .unwrap_or(&modes[0]);
    // the highest rate offered at that size
    let best = modes.iter().filter(|m| m.0 == pick.0 && m.1 == pick.1).max_by(|a, b| a.2.total_cmp(&b.2)).unwrap_or(pick);
    Some((format!("{}x{}", best.0, best.1), best.3.clone()))
}

/// Open a System Settings page (used for camera and screen-recording permission).
pub fn open_settings(url: &str) {
    let _ = Command::new("/usr/bin/open").arg(url).spawn();
}

impl Drop for LiveFeed {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Records the processed live picture to a file while it plays.
pub struct Recorder {
    tx: Option<SyncSender<Img>>,
    handle: Option<std::thread::JoinHandle<Result<(), String>>>,
    pub started: std::time::Instant,
    pub path: PathBuf,
}

impl Recorder {
    pub fn start(out: PathBuf, format: Format, w: usize, h: usize, fps: f32) -> Result<Recorder, String> {
        let (w, h) = (even(w), even(h));
        let mut enc = Encoder::start(&out, format, w, h, fps, None)?;
        let (tx, rx): (SyncSender<Img>, Receiver<Img>) = sync_channel(8);
        let handle = std::thread::spawn(move || {
            while let Ok(img) = rx.recv() {
                enc.write(&img)?;
            }
            enc.finish()
        });
        Ok(Recorder { tx: Some(tx), handle: Some(handle), started: std::time::Instant::now(), path: out })
    }

    /// Hand over a finished frame; dropped if the encoder is behind.
    pub fn push(&self, img: &Img) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(img.clone());
        }
    }

    pub fn finish(mut self) -> Result<PathBuf, String> {
        drop(self.tx.take());
        match self.handle.take().map(|h| h.join()) {
            Some(Ok(Ok(()))) => Ok(self.path.clone()),
            Some(Ok(Err(e))) => Err(e),
            _ => Err("The recorder stopped unexpectedly.".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exports a generated clip (with sound) through a preset and checks the result.
    #[test]
    fn video_export_round_trip() {
        let dir = std::env::temp_dir().join("kiln-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        let ok = Command::new(tool("ffmpeg"))
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30:duration=2", "-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-shortest", "-pix_fmt", "yuv420p"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(ok.success());
        let info = probe(&src).unwrap();
        assert_eq!((info.w, info.h), (640, 360));
        let presets = crate::params::factory_presets();
        let p = presets.iter().find(|p| p.name == "BAD CABLE").unwrap().params.clone();
        let out = dir.join("out.mp4");
        let t0 = std::time::Instant::now();
        let job = start_export(&Source::Video { path: src, info }, p, Vec::new(), Format::H264, out.clone(), 5.0);
        loop {
            if let Some(r) = job.result.lock().unwrap().take() {
                r.unwrap();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        eprintln!("exported 60 frames in {:?}", t0.elapsed());
        let o = probe(&out).unwrap();
        assert_eq!((o.w, o.h), (640, 360));
        assert!(o.has_audio);
        assert!(o.duration > 1.8);
    }

    /// A video feed (here a file played through the live path) arrives and can be recorded.
    #[test]
    fn live_feed_and_recording() {
        let dir = std::env::temp_dir().join("kiln-live-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("feed.mp4");
        assert!(Command::new(tool("ffmpeg"))
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=640x480:rate=30:duration=2", "-pix_fmt", "yuv420p"])
            .arg(&src)
            .status()
            .unwrap()
            .success());
        let Ok(mut feed) = LiveFeed::start(&LiveInput::Stream(src.to_string_lossy().to_string()), 320, 180) else { panic!("feed did not open") };
        let out = dir.join("rec.mp4");
        let rec = Recorder::start(out.clone(), Format::H264, 320, 180, 30.0).unwrap();
        let mut got = 0;
        for _ in 0..200 {
            if let Ok(Some(img)) = feed.latest() {
                assert_eq!((img.w, img.h), (320, 180));
                rec.push(&img);
                got += 1;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(got > 0);
        rec.finish().unwrap();
        assert!(probe(&out).unwrap().w == 320);
    }

    /// The mode list ffmpeg prints for a typical MacBook camera.
    #[test]
    fn camera_mode_from_supported_list() {
        let text = "[avfoundation @ 0x1] Selected framerate (10000.000000) is not supported by the device.\n\
[avfoundation @ 0x1] Supported modes:\n\
[avfoundation @ 0x1]   1920x1080@[1.000000 30.000030]fps\n\
[avfoundation @ 0x1]   1280x720@[1.000000 30.000030]fps\n\
[avfoundation @ 0x1]   1280x720@[1.000000 60.000240]fps\n\
[avfoundation @ 0x1]   640x480@[1.000000 30.000030]fps\n\
[in#0 @ 0x2] Error opening input: Input/output error";
        assert_eq!(pick_mode(text), Some(("1280x720".to_string(), "60.000240".to_string())));
        let no_720 = "[avfoundation @ 0x1]   3840x2160@[1.000000 30.000030]fps\n[avfoundation @ 0x1]   1920x1080@[15.000000 30.000030]fps";
        assert_eq!(pick_mode(no_720), Some(("1920x1080".to_string(), "30.000030".to_string())));
        assert_eq!(pick_mode("Error opening input: Input/output error"), None);
        let e = LiveError::from_ffmpeg(&LiveInput::Camera { index: "0".into(), name: "Camera".into() }, "Error opening input: Input/output error");
        assert!(e.settings.is_some());
    }
}