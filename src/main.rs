mod assets;
mod preview;
mod renderer;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use assets::{Fit, Framing};
use renderer::{Gpu, Renderer};

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
    /// Analyse the WAV into per-frame features (milestone 2).
    Analyse,
    /// Render the full video (milestone 7).
    Render,
}

/// What to show. Replaced by `timeline.toml` cues in milestone 6.
#[derive(Args)]
struct SceneArgs {
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
    fn load(&self, width: u32, height: u32) -> Result<image::RgbaImage> {
        match &self.photo {
            Some(path) => {
                let framing = Framing { fit: self.fit, focus: self.focus };
                assets::load_photo(path, width, height, &framing)
            }
            None => Ok(assets::test_card(width, height)),
        }
    }
}

fn parse_focus(s: &str) -> Result<[f32; 2], String> {
    let parts: Vec<f32> = s
        .split(',')
        .map(|p| p.trim().parse::<f32>().map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    match parts[..] {
        [x, y] if (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) => Ok([x, y]),
        _ => Err("expected `x,y` with both values in 0..1".into()),
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Frame { scene, frame, out, width, height } => {
            let started = Instant::now();
            let photo = scene.load(width, height)?;
            let gpu = Gpu::new(Gpu::instance(), None)?;
            let renderer = Renderer::new(&gpu, &photo, FPS);
            renderer.render(&gpu, frame);
            let image = renderer.read_frame(&gpu)?;
            if let Some(dir) = out.parent() {
                std::fs::create_dir_all(dir)?;
            }
            image.save(&out).with_context(|| format!("writing {}", out.display()))?;
            println!(
                "wrote {} ({width}×{height}, frame {frame}) in {:.2?}",
                out.display(),
                started.elapsed()
            );
            Ok(())
        }
        Command::Preview { scene, width, height } => preview::run(scene.load(width, height)?),
        Command::Analyse => bail!("`analyse` is not implemented yet (milestone 2)"),
        Command::Render => bail!("`render` is not implemented yet (milestone 7)"),
    }
}
