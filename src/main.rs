mod analysis;
mod assets;
mod export;
mod look;
mod params;
mod preview;
mod project;
mod renderer;
mod show;
mod tears;
mod timeline;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use assets::AssetCache;
use project::{Project, ProjectArgs};
use renderer::{Gpu, RenderSettings, Renderer, Shaders};

/// Output frame rate (`[meta] fps` must match).
pub const FPS: f32 = 25.0;

#[derive(Parser)]
#[command(name = "static", about = "Visualiser for \"still, charged\"")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render frames headlessly to PNGs.
    Frame {
        #[command(flatten)]
        project: ProjectArgs,
        /// Frame index to render (at 25 fps).
        #[arg(long, default_value_t = 0)]
        frame: u32,
        /// Render this many consecutive frames (numbered files).
        #[arg(long, default_value_t = 1)]
        count: u32,
        #[arg(short, long, default_value = "out/frame.png")]
        out: PathBuf,
        #[arg(long, default_value_t = 3840)]
        width: u32,
        #[arg(long, default_value_t = 2160)]
        height: u32,
    },
    /// Open the live preview window.
    Preview {
        #[command(flatten)]
        project: ProjectArgs,
        /// Internal render resolution (the window scales it to fit).
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
    },
    /// Analyse a WAV into per-frame features, plus a debug plot.
    Analyse {
        wav: PathBuf,
        /// Features file to write. Default: out/<wav name>.features.bin
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Click sensitivity: how far above the local flux level a click must
        /// rise, in robust standard deviations. Lower = more clicks.
        #[arg(long, default_value_t = analysis::DEFAULT_CLICK_THRESHOLD)]
        click_threshold: f32,
        /// Skip writing the SVG plot next to the features file.
        #[arg(long)]
        no_plot: bool,
    },
    /// Render the video: frames piped to ffmpeg in resumable chunks, joined
    /// and muxed with the audio into an upload-ready MP4.
    Render {
        #[command(flatten)]
        project: ProjectArgs,
        /// Output file (.mp4, or .mov with --audio-copy).
        #[arg(short, long, default_value = "out/still_charged.mp4")]
        out: PathBuf,
        #[arg(long, default_value_t = 3840)]
        width: u32,
        #[arg(long, default_value_t = 2160)]
        height: u32,
        /// Start here: a frame number or a time ("1:30", "90s"). Default: 0.
        #[arg(long, value_parser = export::parse_position)]
        from: Option<u32>,
        /// Stop before here (frame or time). Default: the end of the video.
        #[arg(long, value_parser = export::parse_position)]
        to: Option<u32>,
        /// Length of each resumable chunk.
        #[arg(long, default_value = "60s", value_parser = export::parse_position)]
        chunk: u32,
        #[arg(long, value_enum, default_value_t = export::Codec::H264)]
        codec: export::Codec,
        /// Quality (lower = better and bigger). Default 14 for H.264, 16 for H.265.
        #[arg(long)]
        crf: Option<u8>,
        /// Encoder speed/efficiency preset.
        #[arg(long, default_value = "slow")]
        preset: String,
        /// Lossless 24-bit PCM audio instead of AAC (needs a .mov output).
        #[arg(long)]
        audio_copy: bool,
        /// Keep the chunk files after joining them.
        #[arg(long)]
        keep_chunks: bool,
        /// Instead of the full render: encode an excerpt (--from, 20 s by
        /// default) at YouTube-like bitrates to check how the snow survives.
        #[arg(long)]
        preview_youtube: bool,
    },
}

/// `out/frame.png` → `out/frame_000700.png`
fn numbered(path: &std::path::Path, index: u32) -> PathBuf {
    let stem = path.file_stem().map_or("frame".into(), |s| s.to_string_lossy().into_owned());
    let ext = path.extension().map_or("png".into(), |e| e.to_string_lossy().into_owned());
    path.with_file_name(format!("{stem}_{index:06}.{ext}"))
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Frame { project, frame, count, out, width, height } => {
            let started = Instant::now();
            let (project, show) = Project::load(&project)?;
            let assets = AssetCache::new(width, height).assets(show.timeline());
            let gpu = Gpu::new(Gpu::instance(), None)?;
            let settings = RenderSettings { fps: FPS, seed: project.seed };
            let renderer = Renderer::new(&gpu, &assets, settings, &Shaders::embedded())?;
            if let Some(dir) = out.parent() {
                std::fs::create_dir_all(dir)?;
            }
            for index in frame..frame + count {
                let inputs = show.inputs(index);
                renderer.render(&gpu, &inputs);
                let path = if count == 1 { out.clone() } else { numbered(&out, index) };
                renderer.read_frame(&gpu)?.save(&path).with_context(|| format!("writing {}", path.display()))?;
                let (f, d) = (show.features_at(index), inputs.disturbance);
                println!(
                    "frame {index} ({:.2} s): hiss {:.2} click {:.2} loud {:.2} → snow {:.2} jitter {:.2} tear {:.2}{}{}  {}",
                    index as f32 / FPS,
                    f.hiss,
                    f.click,
                    f.loud,
                    d.snow,
                    d.jitter,
                    d.tear,
                    if inputs.tears.roll > 0.0 { " (roll)" } else { "" },
                    if inputs.title.alpha > 0.0 { " (title)" } else { "" },
                    path.display()
                );
            }
            if show.len().is_some_and(|len| frame + count > len) {
                eprintln!("note: frames past the end of the video render as silence");
            }
            println!("{count} frame(s) at {width}×{height} in {:.2?}", started.elapsed());
            Ok(())
        }
        Command::Preview { project, width, height } => {
            let (project, show) = Project::load(&project)?;
            preview::run(project, show, (width, height))
        }
        Command::Analyse { wav, out, click_threshold, no_plot } => {
            let started = Instant::now();
            let params = analysis::AnalysisParams { fps: FPS, click_threshold };
            let features = analysis::analyse(&wav, &params)?;
            let stem = wav.file_stem().map_or("audio".into(), |s| s.to_string_lossy().into_owned());
            let out = out.unwrap_or_else(|| PathBuf::from(format!("out/{stem}.features.bin")));
            if let Some(dir) = out.parent() {
                std::fs::create_dir_all(dir)?;
            }
            features.save(&out)?;
            println!(
                "analysed {} in {:.2?}: {:.1} s, {} frames, {} clicks",
                wav.display(),
                started.elapsed(),
                features.duration,
                features.frame_count(),
                features.clicks.len()
            );
            println!("wrote {}", out.display());
            if !no_plot {
                let plot = out.with_extension("svg");
                analysis::write_svg(&features, &analysis::SmoothingParams::default(), &stem, &plot)?;
                println!("wrote {}", plot.display());
            }
            Ok(())
        }
        Command::Render {
            project,
            out,
            width,
            height,
            from,
            to,
            chunk,
            codec,
            crf,
            preset,
            audio_copy,
            keep_chunks,
            preview_youtube,
        } => {
            let (project, show) = Project::load(&project)?;
            let opts = export::Options {
                out,
                width,
                height,
                from,
                to,
                chunk_frames: chunk,
                codec,
                crf,
                preset,
                audio_copy,
                keep_chunks,
            };
            if preview_youtube {
                export::preview_youtube(&project, &show, &opts)
            } else {
                export::render(&project, &show, &opts)
            }
        }
    }
}
