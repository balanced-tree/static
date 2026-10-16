use std::path::Path;

use anyhow::{Context, Result, bail};

/// Mono samples in -1..1.
pub struct Audio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Audio {
    pub fn duration(&self) -> f32 {
        self.samples.len() as f32 / self.sample_rate as f32
    }
}

/// Reads any PCM WAV (8–32-bit int or 32-bit float) and averages the
/// channels to mono.
pub fn read_mono(path: &Path) -> Result<Audio> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening WAV {}", path.display()))?;
    let spec = reader.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    let channels = spec.channels as usize;
    if channels == 0 || interleaved.is_empty() {
        bail!("{} contains no audio", path.display());
    }
    let samples = interleaved.chunks_exact(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect();
    Ok(Audio { samples, sample_rate: spec.sample_rate })
}
