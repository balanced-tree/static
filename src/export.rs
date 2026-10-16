//! Final render (PLAN §9): headless frames piped into `ffmpeg`, encoded in
//! chunks that are joined without re-encoding, with the original audio muxed
//! in.
//!
//! Chunks are what make long renders resumable: each one is written under a
//! temporary name and renamed only once ffmpeg has finished it, so after an
//! interruption every chunk that exists is complete and is skipped. A
//! fingerprint of everything that affects the picture guards against reusing
//! chunks rendered from an older timeline, photo or shader.

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};

use crate::FPS;
use crate::assets::AssetCache;
use crate::project::Project;
use crate::renderer::{Gpu, RenderSettings, Renderer, Shaders};
use crate::show::Show;
use crate::timeline::parse_time;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Codec {
    H264,
    H265,
}

pub struct Options {
    pub out: PathBuf,
    pub width: u32,
    pub height: u32,
    /// First frame (inclusive) and last (exclusive); `None` = whole video.
    pub from: Option<u32>,
    pub to: Option<u32>,
    /// Frames per chunk.
    pub chunk_frames: u32,
    pub codec: Codec,
    /// `None` = the codec's default.
    pub crf: Option<u8>,
    pub preset: String,
    /// Lossless PCM audio instead of AAC (needs a `.mov`).
    pub audio_copy: bool,
    pub keep_chunks: bool,
}

/// `--from` / `--to`: a time (`"1:30"`, `"90s"`) or a bare frame number.
pub fn parse_position(s: &str) -> Result<u32, String> {
    let s = s.trim();
    if s.contains(':') || s.ends_with('s') {
        let secs = parse_time(s).map_err(|e| e.to_string())?;
        if secs < 0.0 {
            return Err("must not be negative".into());
        }
        Ok((secs * FPS as f64).round() as u32)
    } else {
        s.parse().map_err(|_| format!("expected a frame number or a time like \"1:30\", got {s:?}"))
    }
}

/// The frame range to render, checked against the video's length.
fn range(show: &Show, from: Option<u32>, to: Option<u32>) -> Result<(u32, u32)> {
    let len = show.len();
    let to = match (to, len) {
        (Some(to), _) => to,
        (None, Some(len)) => len,
        (None, None) => bail!("without audio the video has no length; give --to"),
    };
    let from = from.unwrap_or(0);
    ensure!(from < to, "nothing to render: --from ({from}) is not before --to ({to})");
    if let Some(len) = len
        && to > len
    {
        eprintln!("note: the video is {len} frames long; frames past that render as silence");
    }
    Ok((from, to))
}

fn secs(frame: u32) -> f64 {
    frame as f64 / FPS as f64
}

fn check_ffmpeg() -> Result<()> {
    match Command::new("ffmpeg").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status() {
        Ok(s) if s.success() => Ok(()),
        _ => bail!("ffmpeg not found on PATH (macOS: `brew install ffmpeg`)"),
    }
}

/// The configured audio, which must exist for a render.
fn audio_path(project: &Project) -> Result<Option<&Path>> {
    match &project.audio_path {
        Some(p) if !p.exists() => bail!("audio {} not found", p.display()),
        Some(p) => Ok(Some(p)),
        None => {
            eprintln!("warning: no audio configured; the video will be silent");
            Ok(None)
        }
    }
}

/// Raw RGBA in on stdin, converted to YUV with the BT.709 matrix (ffmpeg's
/// default for RGB input is BT.601, which would shift colours) and tagged
/// to match.
fn raw_input_args(width: u32, height: u32) -> Vec<String> {
    let size = format!("{width}x{height}");
    let mut args: Vec<String> =
        ["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba"].map(String::from).into();
    args.extend(["-s".into(), size, "-framerate".into(), FPS.to_string(), "-i".into(), "-".into()]);
    args
}

const COLOUR_ARGS: [&str; 8] =
    ["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"];
const TO_YUV: &str = "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p";

impl Options {
    fn video_args(&self) -> Vec<String> {
        let (encoder, default_crf, extra): (&str, u8, &[&str]) = match self.codec {
            // hvc1 tag: plays in QuickTime/Safari and uploads fine.
            Codec::H265 => ("libx265", 16, &["-tag:v", "hvc1", "-x265-params", "log-level=error"]),
            Codec::H264 => ("libx264", 14, &[]),
        };
        let crf = self.crf.unwrap_or(default_crf).to_string();
        let mut args: Vec<String> =
            ["-vf", TO_YUV, "-c:v", encoder, "-preset", &self.preset, "-crf", &crf].map(String::from).into();
        // YouTube's recommendation: closed GOP of half the frame rate, 2 B-frames.
        args.extend(["-g", "12", "-bf", "2"].map(String::from));
        args.extend(extra.iter().map(|s| s.to_string()));
        args.extend(COLOUR_ARGS.map(String::from));
        args
    }

    fn audio_args(&self) -> Vec<String> {
        if self.audio_copy {
            ["-c:a", "pcm_s24le"].map(String::from).into()
        } else {
            ["-c:a", "aac", "-b:a", "384k", "-ar", "48000"].map(String::from).into()
        }
    }
}

/// Everything that changes the rendered chunks. Files are fingerprinted by
/// size and modification time, so editing one invalidates old chunks.
fn fingerprint(project: &Project, show: &Show, opts: &Options) -> String {
    let mut h = DefaultHasher::new();
    env!("CARGO_PKG_VERSION").hash(&mut h);
    Shaders::embedded().hash(&mut h);
    (opts.width, opts.height, opts.chunk_frames, opts.video_args()).hash(&mut h);
    project.seed.hash(&mut h);
    let mut file = |path: &Path| {
        path.hash(&mut h);
        if let Ok(m) = fs::metadata(path) {
            m.len().hash(&mut h);
            m.modified().ok().hash(&mut h);
        }
    };
    if let Some(p) = &project.timeline_path {
        file(p);
    }
    if let Some(p) = &project.audio_path {
        file(p);
    }
    for photo in &show.timeline().photos {
        file(&photo.path);
    }
    if let Some(title) = &show.timeline().title {
        file(&title.font);
    }
    format!("{:016x}", h.finish())
}

/// Renders frames `from..to` and hands each one, as raw RGBA, to `sink`.
/// Readback and piping run on another thread, so the GPU keeps working while
/// ffmpeg takes the previous frame.
struct FrameSource<'a> {
    gpu: Gpu,
    renderer: Renderer,
    show: &'a Show,
}

impl<'a> FrameSource<'a> {
    fn new(project: &Project, show: &'a Show, width: u32, height: u32) -> Result<Self> {
        ensure!(width % 2 == 0 && height % 2 == 0, "width and height must be even for yuv420p");
        let assets = AssetCache::new(width, height).assets(show.timeline());
        let gpu = Gpu::new(Gpu::instance(), None)?;
        let settings = RenderSettings { fps: FPS, seed: project.seed };
        let renderer = Renderer::new(&gpu, &assets, settings, &Shaders::embedded())?;
        Ok(Self { gpu, renderer, show })
    }

    fn pipe(&self, from: u32, to: u32, stdin: ChildStdin, progress: &mut Progress) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(3);
        let writer = thread::spawn(move || -> std::io::Result<()> {
            let mut stdin = stdin;
            for frame in rx {
                stdin.write_all(&frame)?;
            }
            Ok(())
        });
        for index in from..to {
            self.renderer.render(&self.gpu, &self.show.inputs(index));
            let frame = self.renderer.read_frame(&self.gpu)?.into_raw();
            if tx.send(frame).is_err() {
                break; // the writer stopped: ffmpeg exited; its status says why
            }
            progress.tick();
        }
        drop(tx);
        writer.join().expect("writer thread panicked").context("piping frames to ffmpeg")?;
        Ok(())
    }
}

struct Progress {
    done: u32,
    total: u32,
    started: Instant,
    last: Instant,
}

impl Progress {
    fn new(total: u32) -> Self {
        Self { done: 0, total, started: Instant::now(), last: Instant::now() }
    }

    fn skip(&mut self, frames: u32) {
        self.total -= frames;
    }

    fn tick(&mut self) {
        self.done += 1;
        if self.last.elapsed().as_secs_f32() >= 1.0 || self.done == self.total {
            self.last = Instant::now();
            let elapsed = self.started.elapsed().as_secs_f64();
            let fps = self.done as f64 / elapsed.max(1e-9);
            let left = (self.total - self.done) as f64 / fps.max(1e-9);
            eprint!(
                "\r  {}/{} frames · {fps:.1} fps · {} elapsed · ~{} left   ",
                self.done,
                self.total,
                hms(elapsed),
                hms(left)
            );
            if self.done == self.total {
                eprintln!();
            }
        }
    }
}

fn hms(secs: f64) -> String {
    let s = secs.round() as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Starts ffmpeg with `args`, its stderr going to `log`.
fn spawn_ffmpeg(args: &[String], log: &Path) -> Result<Child> {
    let log_file = File::create(log).with_context(|| format!("creating {}", log.display()))?;
    Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(log_file)
        .spawn()
        .context("starting ffmpeg")
}

fn wait_ffmpeg(mut child: Child, log: &Path, what: &str) -> Result<()> {
    let status = child.wait().context("waiting for ffmpeg")?;
    if !status.success() {
        let tail = fs::read_to_string(log).unwrap_or_default();
        let tail: Vec<_> = tail.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect();
        bail!("ffmpeg failed {what} ({status}); see {}:\n{}", log.display(), tail.join("\n"));
    }
    Ok(())
}

/// The full render: chunks, then join and mux.
pub fn render(project: &Project, show: &Show, opts: &Options) -> Result<()> {
    check_ffmpeg()?;
    let audio = audio_path(project)?;
    let ext = opts.out.extension().and_then(|e| e.to_str()).unwrap_or("");
    ensure!(matches!(ext, "mp4" | "mov"), "output must be .mp4 or .mov, got {}", opts.out.display());
    ensure!(!opts.audio_copy || ext == "mov", "--audio-copy needs a .mov output (MP4 can't hold PCM audio)");
    ensure!(opts.chunk_frames > 0, "--chunk must be longer than zero");
    let (from, to) = range(show, opts.from, opts.to)?;
    if let Some(dir) = opts.out.parent() {
        fs::create_dir_all(dir)?;
    }

    // Chunk directory, cleared if it holds chunks of a different render.
    let chunks_dir = opts.out.with_extension("chunks");
    fs::create_dir_all(&chunks_dir)?;
    let print = fingerprint(project, show, opts);
    let print_file = chunks_dir.join("fingerprint");
    if fs::read_to_string(&print_file).is_ok_and(|p| p.trim() != print) {
        eprintln!("the timeline, media or settings changed since the last chunks were rendered; starting over");
        for entry in fs::read_dir(&chunks_dir)? {
            fs::remove_file(entry?.path())?;
        }
    }
    fs::write(&print_file, &print)?;

    // Chunks sit on absolute boundaries, so different ranges share them.
    let mut chunks = Vec::new();
    let mut start = from;
    while start < to {
        let end = ((start / opts.chunk_frames + 1) * opts.chunk_frames).min(to);
        chunks.push((start, end, chunks_dir.join(format!("{start:06}-{end:06}.mp4"))));
        start = end;
    }

    eprintln!(
        "rendering frames {from}..{to} ({} – {}) at {}×{} in {} chunk(s) → {}",
        hms(secs(from)),
        hms(secs(to)),
        opts.width,
        opts.height,
        chunks.len(),
        opts.out.display()
    );
    let mut progress = Progress::new(to - from);
    let todo: Vec<_> = chunks.iter().filter(|(_, _, path)| !path.exists()).collect();
    let reused = chunks.len() - todo.len();
    if reused > 0 {
        eprintln!("  resuming: {reused} chunk(s) already done");
        progress.skip(chunks.iter().filter(|(_, _, p)| p.exists()).map(|(s, e, _)| e - s).sum());
    }
    if !todo.is_empty() {
        let source = FrameSource::new(project, show, opts.width, opts.height)?;
        for (start, end, path) in todo {
            let part = path.with_extension("part");
            let log = chunks_dir.join("ffmpeg.log");
            let mut args = raw_input_args(opts.width, opts.height);
            args.extend(opts.video_args());
            args.extend(["-an", "-f", "mp4"].map(String::from));
            args.push(part.to_string_lossy().into_owned());
            let mut child = spawn_ffmpeg(&args, &log)?;
            let stdin = child.stdin.take().expect("piped stdin");
            let piped = source.pipe(*start, *end, stdin, &mut progress);
            wait_ffmpeg(child, &log, &format!("on chunk {}", path.display()))?;
            piped?;
            fs::rename(&part, path)?;
        }
    }

    // Join (stream copy) and mux the audio for the rendered range.
    eprintln!("joining chunks and muxing audio");
    let list = chunks_dir.join("chunks.txt");
    let mut text = String::new();
    for (_, _, path) in &chunks {
        let abs = fs::canonicalize(path)?;
        text.push_str(&format!("file '{}'\n", abs.to_string_lossy().replace('\'', r"'\''")));
    }
    fs::write(&list, text)?;
    let mut args: Vec<String> =
        ["-hide_banner", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i"].map(String::from).into();
    args.push(list.to_string_lossy().into_owned());
    if let Some(audio) = audio {
        args.extend(["-ss".into(), secs(from).to_string(), "-t".into(), secs(to - from).to_string(), "-i".into()]);
        args.push(audio.to_string_lossy().into_owned());
        args.extend(["-map", "0:v", "-map", "1:a"].map(String::from));
        args.extend(opts.audio_args());
    }
    args.extend(["-c:v", "copy", "-movflags", "+faststart"].map(String::from));
    args.push(opts.out.to_string_lossy().into_owned());
    let log = chunks_dir.join("ffmpeg-mux.log");
    let mut child = spawn_ffmpeg(&args, &log)?;
    drop(child.stdin.take());
    wait_ffmpeg(child, &log, "joining the chunks")?;

    if !opts.keep_chunks {
        fs::remove_dir_all(&chunks_dir)?;
    }
    let size = fs::metadata(&opts.out).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "wrote {} ({:.1} MB, {:.1} Mbit/s average)",
        opts.out.display(),
        size as f64 / 1e6,
        size as f64 * 8.0 / 1e6 / secs(to - from).max(1e-9)
    );
    Ok(())
}

/// Roughly how YouTube will treat the upload: an excerpt encoded once at
/// typical YouTube bitrates for 1080p and 2160p, so you can check that the
/// snow survives (YouTube uses VP9/AV1, which do somewhat better than x264
/// at the same bitrate, so this errs on the harsh side).
pub fn preview_youtube(project: &Project, show: &Show, opts: &Options) -> Result<()> {
    check_ffmpeg()?;
    let audio = audio_path(project)?;
    let from = opts.from.unwrap_or(0);
    let to = opts.to.unwrap_or(from + (20.0 * FPS) as u32);
    let (from, to) = range(show, Some(from), Some(to))?;
    if let Some(dir) = opts.out.parent() {
        fs::create_dir_all(dir)?;
    }
    let stem = opts.out.with_extension("");
    let out = |label: &str| PathBuf::from(format!("{}.youtube-{label}.mp4", stem.display()));
    let (hd, uhd) = (out("1080p"), out("2160p"));

    let mut args = raw_input_args(opts.width, opts.height);
    if let Some(audio) = audio {
        args.extend(["-ss".into(), secs(from).to_string(), "-t".into(), secs(to - from).to_string(), "-i".into()]);
        args.push(audio.to_string_lossy().into_owned());
    }
    for (path, height, rate) in [(&hd, 1080, "8M"), (&uhd, 2160, "25M")] {
        let filter = format!("scale=-2:{height}:flags=lanczos:out_color_matrix=bt709:out_range=tv,format=yuv420p");
        args.extend(["-map".into(), "0:v".into(), "-vf".into(), filter]);
        args.extend(
            ["-c:v", "libx264", "-preset", "medium", "-b:v", rate, "-maxrate", rate, "-bufsize", rate]
                .map(String::from),
        );
        args.extend(COLOUR_ARGS.map(String::from));
        if audio.is_some() {
            args.extend(["-map", "1:a", "-c:a", "aac", "-b:a", "128k"].map(String::from));
        }
        args.push(path.to_string_lossy().into_owned());
    }

    eprintln!("encoding frames {from}..{to} at YouTube-like bitrates (1080p 8 Mbit/s, 2160p 25 Mbit/s)");
    let log = stem.with_extension("youtube.log");
    let source = FrameSource::new(project, show, opts.width, opts.height)?;
    let mut child = spawn_ffmpeg(&args, &log)?;
    let stdin = child.stdin.take().expect("piped stdin");
    let mut progress = Progress::new(to - from);
    let piped = source.pipe(from, to, stdin, &mut progress);
    wait_ffmpeg(child, &log, "on the YouTube preview")?;
    piped?;
    let _ = fs::remove_file(&log);
    eprintln!("wrote {} and {}", hd.display(), uhd.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_are_frames_or_times() {
        assert_eq!(parse_position("1500"), Ok(1500));
        assert_eq!(parse_position("1:00"), Ok(1500));
        assert_eq!(parse_position("2.5s"), Ok(63));
        assert!(parse_position("-1s").is_err());
        assert!(parse_position("soon").is_err());
    }
}
