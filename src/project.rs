//! Loading a piece: the timeline (from `timeline.toml`, or a one-photo
//! timeline built from flags), the audio and its features.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, bail};
use clap::Args;

use crate::FPS;
use crate::analysis::{self, Features, Interleaved};
use crate::assets::{Fit, Framing};
use crate::look::{Stillness, StillnessMode};
use crate::show::Show;
use crate::timeline::{PhotoSpec, Timeline, TimelineFile};

/// Used when `--timeline` isn't given and nothing photo-specific is either.
const DEFAULT_TIMELINE: &str = "timeline.toml";

#[derive(Args)]
pub struct ProjectArgs {
    /// The timeline. Defaults to ./timeline.toml if it exists (unless
    /// `--photo` is given).
    #[arg(long)]
    timeline: Option<PathBuf>,
    /// The WAV; overrides `[meta] audio`. Analysed on the fly if there is no
    /// features file.
    #[arg(long)]
    audio: Option<PathBuf>,
    /// Features file from `analyse`; overrides `[meta] features`.
    #[arg(long)]
    features: Option<PathBuf>,
    /// Seed for all noise; overrides `[meta] seed`.
    #[arg(long)]
    seed: Option<u32>,
    /// Without a timeline: the photo to show (omit for a test card).
    #[arg(long)]
    photo: Option<PathBuf>,
    /// Without a timeline: `fill` crops to 16:9, `contain` letterboxes.
    #[arg(long, value_enum)]
    fit: Option<Fit>,
    /// Without a timeline: crop centre as normalised `x,y` (used by `fill`).
    #[arg(long, value_parser = parse_focus)]
    focus: Option<[f32; 2]>,
    /// Without a timeline: how signal energy maps to disturbance.
    #[arg(long, value_enum)]
    stillness: Option<StillnessMode>,
}

pub struct Project {
    /// `None` when running from flags.
    pub timeline_path: Option<PathBuf>,
    pub audio: Option<Interleaved>,
    /// The WAV as configured (flag or `[meta] audio`), whether or not it
    /// exists; the export muxes this file.
    pub audio_path: Option<PathBuf>,
    pub seed: u32,
    seed_override: Option<u32>,
    /// Length of the audio (or features), which the timeline is placed against.
    audio_secs: Option<f64>,
}

impl Project {
    /// Loads everything; returns the project and the show built from it.
    pub fn load(args: &ProjectArgs) -> Result<(Self, Show)> {
        let flags_only = args.photo.is_some() || args.fit.is_some() || args.focus.is_some() || args.stillness.is_some();
        let timeline_path = match &args.timeline {
            Some(path) if flags_only => {
                bail!("--photo, --fit, --focus and --stillness only apply without a timeline ({})", path.display())
            }
            Some(path) => Some(path.clone()),
            None if !flags_only && Path::new(DEFAULT_TIMELINE).exists() => {
                eprintln!("using {DEFAULT_TIMELINE}");
                Some(PathBuf::from(DEFAULT_TIMELINE))
            }
            None => None,
        };
        let mut file = match &timeline_path {
            Some(path) => TimelineFile::load(path)?,
            None => {
                let photo = args.photo.as_ref().map(|path| PhotoSpec {
                    path: path.clone(),
                    framing: Framing {
                        fit: args.fit.unwrap_or(Fit::Fill),
                        focus: args.focus.unwrap_or([0.5, 0.5]),
                        ..Default::default()
                    },
                    soften: 0.0,
                });
                let stillness = Stillness::preset(args.stillness.unwrap_or(StillnessMode::StillAtRest));
                TimelineFile::single(photo, stillness, 1234)
            }
        };
        if let Some(seed) = args.seed {
            file.seed = seed;
        }

        // Flags must work; files named in the timeline may still be missing
        // while it is being written.
        let audio = match (&args.audio, &file.audio) {
            (Some(path), _) => Some(analysis::read_wav(path)?),
            (None, Some(path)) if path.exists() => Some(analysis::read_wav(path)?),
            (None, Some(path)) => {
                eprintln!("warning: audio {} not found; previewing silently", path.display());
                None
            }
            (None, None) => None,
        };
        let features = match (&args.features, &file.features, &audio) {
            (Some(path), _, _) => Some(Features::load(path)?),
            (None, Some(path), _) if path.exists() => Some(Features::load(path)?),
            (None, _, Some(audio)) => {
                let started = Instant::now();
                let params = analysis::AnalysisParams { fps: FPS, click_threshold: analysis::DEFAULT_CLICK_THRESHOLD };
                let features = analysis::analyse_audio(&audio.to_mono(), &params);
                eprintln!("analysed audio in {:.2?} ({} clicks)", started.elapsed(), features.clicks.len());
                Some(features)
            }
            (None, _, None) => None,
        };
        if let (Some(f), Some(a)) = (&features, &audio)
            && (f.duration as f64 - a.duration()).abs() > 0.1
        {
            eprintln!(
                "warning: features are {:.1} s long but the audio is {:.1} s — made from a different file?",
                f.duration,
                a.duration()
            );
        }
        let audio_secs = audio.as_ref().map(|a| a.duration()).or(features.as_ref().map(|f| f.duration as f64));

        let audio_path = args.audio.clone().or(file.audio.clone());
        let project = Self { timeline_path, audio, audio_path, seed: file.seed, seed_override: args.seed, audio_secs };
        let show = Show::new(Timeline::compile(&file, audio_secs), features, FPS);
        Ok((project, show))
    }

    /// Re-reads the timeline file (the preview's hot reload). Audio and
    /// features stay as loaded.
    pub fn reload(&mut self) -> Result<Option<Timeline>> {
        let Some(path) = &self.timeline_path else { return Ok(None) };
        let mut file = TimelineFile::load(path)?;
        if let Some(seed) = self.seed_override {
            file.seed = seed;
        }
        self.seed = file.seed;
        Ok(Some(Timeline::compile(&file, self.audio_secs)))
    }
}

fn parse_focus(s: &str) -> Result<[f32; 2], String> {
    let parts: Vec<f32> =
        s.split(',').map(|p| p.trim().parse::<f32>().map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
    match parts[..] {
        [x, y] if (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) => Ok([x, y]),
        _ => Err("expected `x,y` with both values in 0..1".into()),
    }
}
