mod analysis;
mod assets;
mod look;
mod preview;
mod renderer;
mod show;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use assets::{Fit, Framing};
use look::{Look, StillnessMode};
use renderer::{Gpu, RenderSettings, Renderer};
use show::Show;

/// Output frame rate. Moves into `timeline.toml` with milestone 6.
pub const FPS: f32 = 25.0;

#[derive(Parser)]
#[command(name = "static", about = "Visualiser for \"still, charged\"")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render a single frame headlessly to a PNG.
    Frame {
        #[command(flatten)]
        scene: SceneArgs,
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
        scene: SceneArgs,
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
        #[arg(long, default_value_t = 4.0)]
        click_threshold: f32,
        /// Skip writing the SVG plot next to the features file.
        #[arg(long)]
        no_plot: bool,
    },
    /// Render the full video (milestone 7).
    Render,
}

/// What to show and how it reacts. Replaced by `timeline.toml` in milestone 6.
#[derive(Args)]
struct SceneArgs {
    /// Features file from `analyse`. Without it the photo stays at rest.
    #[arg(long)]
    features: Option<PathBuf>,
    /// How signal energy maps to disturbance.
    #[arg(long, value_enum, default_value_t = StillnessMode::StillAtRest)]
    stillness: StillnessMode,
    /// Seed for all noise; change it for a different (but repeatable) take.
    #[arg(long, default_value_t = 1234)]
    seed: u32,
    /// Rows of the snow grid: lower = chunkier snow.
    #[arg(long, default_value_t = 540.0)]
    snow_rows: f32,
    /// Photo to show. Omit to use a generated test card.
    #[arg(long)]
    photo: Option<PathBuf>,
    /// `fill` crops to 16:9, `contain` letterboxes.
    #[arg(long, value_enum, default_value_t = Fit::Fill)]
    fit: Fit,
    /// Crop centre as normalised `x,y` (used by `fill`).
    #[arg(long, value_parser = parse_focus, default_value = "0.5,0.5")]
    focus: [f32; 2],
}

impl SceneArgs {
    fn settings(&self) -> RenderSettings {
        RenderSettings { fps: FPS, seed: self.seed, snow_rows: self.snow_rows }
    }

    fn show(&self) -> Result<Show> {
        let features = self.features.as_deref().map(analysis::Features::load).transpose()?;
        Ok(Show::new(features.as_ref(), Look::new(self.stillness)))
    }

    fn photo(&self, width: u32, height: u32) -> Result<image::RgbaImage> {
        match &self.photo {
            Some(path) => {
                let framing = Framing { fit: self.fit, focus: self.focus };
                assets::load_photo(path, width, height, &framing)
            }
            None => Ok(assets::test_card(width, height)),
        }
    }
}

/// `out/frame.png` → `out/frame_000700.png`
fn numbered(path: &std::path::Path, index: u32) -> PathBuf {
    let stem = path.file_stem().map_or("frame".into(), |s| s.to_string_lossy().into_owned());
    let ext = path.extension().map_or("png".into(), |e| e.to_string_lossy().into_owned());
    path.with_file_name(format!("{stem}_{index:06}.{ext}"))
}

fn parse_focus(s: &str) -> Result<[f32; 2], String> {
    let parts: Vec<f32> =
        s.split(',').map(|p| p.trim().parse::<f32>().map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
    match parts[..] {
        [x, y] if (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) => Ok([x, y]),
        _ => Err("expected `x,y` with both values in 0..1".into()),
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Frame { scene, frame, count, out, width, height } => {
            let started = Instant::now();
            let show = scene.show()?;
            let gpu = Gpu::new(Gpu::instance(), None)?;
            let renderer = Renderer::new(&gpu, &scene.photo(width, height)?, scene.settings());
            if let Some(dir) = out.parent() {
                std::fs::create_dir_all(dir)?;
            }
            for index in frame..frame + count {
                let inputs = show.inputs(index);
                renderer.render(&gpu, &inputs);
                let path = if count == 1 { out.clone() } else { numbered(&out, index) };
                renderer.read_frame(&gpu)?.save(&path).with_context(|| format!("writing {}", path.display()))?;
                let (f, d) = (show.features(index), inputs.disturbance);
                println!(
                    "frame {index} ({:.2} s): hiss {:.2} click {:.2} loud {:.2} → snow {:.2} jitter {:.2}  {}",
                    index as f32 / FPS,
                    f.hiss,
                    f.click,
                    f.loud,
                    d.snow,
                    d.jitter,
                    path.display()
                );
            }
            if show.len().is_some_and(|len| frame + count > len) {
                eprintln!("note: frames past the end of the audio render as silence");
            }
            println!("{count} frame(s) at {width}×{height} in {:.2?}", started.elapsed());
            Ok(())
        }
        Command::Preview { scene, width, height } => {
            preview::run(scene.photo(width, height)?, scene.show()?, scene.settings())
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
        Command::Render => bail!("`render` is not implemented yet (milestone 7)"),
    }
}
