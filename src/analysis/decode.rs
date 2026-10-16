use std::path::Path;

use anyhow::{Context, Result, bail};

/// Mono samples in -1..1, for analysis.
pub struct Audio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Audio {
    pub fn duration(&self) -> f32 {
        self.samples.len() as f32 / self.sample_rate as f32
    }
}

/// The file's samples with channels kept, for playback.
pub struct Interleaved {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}

impl Interleaved {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    pub fn duration(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }

    /// Averages the channels.
    pub fn to_mono(&self) -> Audio {
        let channels = self.channels as usize;
        let samples = self.samples.chunks_exact(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect();
        Audio { samples, sample_rate: self.sample_rate }
    }
}

/// Reads any PCM WAV (8–32-bit int or 32-bit float).
pub fn read_wav(path: &Path) -> Result<Interleaved> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening WAV {}", path.display()))?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    if spec.channels == 0 || samples.len() < spec.channels as usize {
        bail!("{} contains no audio", path.display());
    }
    Ok(Interleaved { samples, channels: spec.channels, sample_rate: spec.sample_rate })
}
