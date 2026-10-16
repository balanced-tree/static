//! Normalised per-frame features, their smoothing, and the `.features.bin`
//! file format.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use super::Audio;
use super::onsets::percentile_sorted;
use super::spectral::Measures;

/// Frames quieter than this count as silence: every feature reads 0 and they
/// are left out of normalisation.
pub const SILENCE_DB: f32 = -70.0;
/// Normalisation maps these percentiles of the piece's non-silent frames to
/// 0 and 1.
const NORM_LO: f32 = 0.05;
const NORM_HI: f32 = 0.95;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClickEvent {
    /// Seconds from the start of the audio.
    pub time: f32,
    /// 0..1, relative to the piece's strong clicks.
    pub strength: f32,
}

/// Analysis output. All series hold one value per video frame, normalised to
/// 0..1 but not smoothed.
#[derive(Debug, PartialEq)]
pub struct Features {
    pub fps: f32,
    pub sample_rate: u32,
    pub duration: f32,
    pub loud: Vec<f32>,
    pub hiss: Vec<f32>,
    /// The two components of `hiss`, kept for the debug plot.
    pub flatness: Vec<f32>,
    pub hiband: Vec<f32>,
    /// Mains hum (50 Hz harmonics). Optional, off unless a cue enables it.
    pub hum: Vec<f32>,
    pub clicks: Vec<ClickEvent>,
}

/// Smoothed feature values for one frame, as the shaders see them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FeatureFrame {
    pub hiss: f32,
    pub click: f32,
    pub loud: f32,
    pub hum: f32,
}

/// One-pole attack/release follower; times in seconds.
#[derive(Clone, Copy, Debug)]
pub struct Envelope {
    pub attack: f32,
    pub release: f32,
}

impl Envelope {
    pub fn apply(&self, xs: &[f32], fps: f32) -> Vec<f32> {
        let coeff = |secs: f32| if secs > 0.0 { (-1.0 / (secs * fps)).exp() } else { 0.0 };
        let (a, r) = (coeff(self.attack), coeff(self.release));
        let mut y = 0.0;
        xs.iter()
            .map(|&x| {
                let c = if x > y { a } else { r };
                y = x + c * (y - x);
                y
            })
            .collect()
    }
}

/// Defaults until `timeline.toml` makes these tunable (milestone 6).
#[derive(Clone, Copy, Debug)]
pub struct SmoothingParams {
    pub loud: Envelope,
    pub hiss: Envelope,
    pub hum: Envelope,
    /// Seconds for a click's envelope to fall to 1/e.
    pub click_decay: f32,
}

impl Default for SmoothingParams {
    fn default() -> Self {
        Self {
            loud: Envelope { attack: 0.05, release: 0.3 },
            hiss: Envelope { attack: 0.15, release: 0.6 },
            hum: Envelope { attack: 0.5, release: 1.0 },
            click_decay: 0.12,
        }
    }
}

impl Features {
    pub fn from_measures(m: &Measures, clicks: Vec<ClickEvent>, fps: f32, audio: &Audio) -> Self {
        let active: Vec<bool> = m.rms_db.iter().map(|&db| db > SILENCE_DB).collect();
        let loud = normalise(&m.rms_db, &active);
        let flatness = normalise(&m.flatness, &active);
        let hiband = normalise(&m.hiband_db, &active);
        let hum = normalise(&m.hum_db, &active);
        // Geometric mean: hiss needs both a noise-like spectrum *and* energy
        // up high, so a loud tone or a dull rumble doesn't read as static.
        let hiss = flatness.iter().zip(&hiband).map(|(f, h)| (f * h).sqrt()).collect();
        Self {
            fps,
            sample_rate: audio.sample_rate,
            duration: audio.duration(),
            loud,
            hiss,
            flatness,
            hiband,
            hum,
            clicks,
        }
    }

    pub fn frame_count(&self) -> usize {
        self.loud.len()
    }

    /// Smoothed values for every frame.
    pub fn frames(&self, p: &SmoothingParams) -> Vec<FeatureFrame> {
        let loud = p.loud.apply(&self.loud, self.fps);
        let hiss = p.hiss.apply(&self.hiss, self.fps);
        let hum = p.hum.apply(&self.hum, self.fps);
        let click = self.click_envelope(p.click_decay);
        (0..self.frame_count())
            .map(|i| FeatureFrame { hiss: hiss[i], click: click[i], loud: loud[i], hum: hum[i] })
            .collect()
    }

    /// Instant attack, exponential decay. A click anywhere inside a frame's
    /// interval shows in that frame at full strength.
    pub fn click_envelope(&self, decay: f32) -> Vec<f32> {
        let per_frame = (-1.0 / (decay.max(1e-3) * self.fps)).exp();
        let mut next = 0;
        let mut env = 0.0f32;
        (0..self.frame_count())
            .map(|i| {
                env *= per_frame;
                let (mid, end) = ((i as f32 + 0.5) / self.fps, (i + 1) as f32 / self.fps);
                while next < self.clicks.len() && self.clicks[next].time < end {
                    let c = self.clicks[next];
                    env = env.max(c.strength * (-(mid - c.time).max(0.0) / decay).exp());
                    next += 1;
                }
                env
            })
            .collect()
    }

    const MAGIC: &[u8; 8] = b"STATICF\0";
    const VERSION: u32 = 1;

    /// Little-endian: header, five f32 series of `frames` values (loud, hiss,
    /// flatness, hiband, hum), then (time, strength) pairs for the clicks.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut w = BufWriter::new(File::create(path).with_context(|| format!("creating {}", path.display()))?);
        w.write_all(Self::MAGIC)?;
        for v in [Self::VERSION, self.sample_rate, self.frame_count() as u32, self.clicks.len() as u32] {
            w.write_all(&v.to_le_bytes())?;
        }
        for v in [self.fps, self.duration] {
            w.write_all(&v.to_le_bytes())?;
        }
        for series in [&self.loud, &self.hiss, &self.flatness, &self.hiband, &self.hum] {
            for v in series {
                w.write_all(&v.to_le_bytes())?;
            }
        }
        for c in &self.clicks {
            w.write_all(&c.time.to_le_bytes())?;
            w.write_all(&c.strength.to_le_bytes())?;
        }
        w.flush()?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let mut r = Reader { bytes: &bytes, pos: 0 };
        if r.take(8)? != Self::MAGIC {
            bail!("{} is not a features file", path.display());
        }
        let version = r.u32()?;
        ensure!(version == Self::VERSION, "{} is version {version}; re-run `static analyse`", path.display());
        let (sample_rate, frames, clicks) = (r.u32()?, r.u32()? as usize, r.u32()? as usize);
        let (fps, duration) = (r.f32()?, r.f32()?);
        let mut series = || (0..frames).map(|_| r.f32()).collect::<Result<Vec<_>>>();
        let (loud, hiss, flatness, hiband, hum) = (series()?, series()?, series()?, series()?, series()?);
        let clicks =
            (0..clicks).map(|_| Ok(ClickEvent { time: r.f32()?, strength: r.f32()? })).collect::<Result<Vec<_>>>()?;
        ensure!(r.pos == bytes.len(), "{} has trailing data", path.display());
        Ok(Self { fps, sample_rate, duration, loud, hiss, flatness, hiband, hum, clicks })
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let s = self.bytes.get(self.pos..self.pos + n).context("features file is truncated")?;
        self.pos += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into()?))
    }
}

/// Maps the `NORM_LO`..`NORM_HI` percentiles of the active frames to 0..1.
fn normalise(xs: &[f32], active: &[bool]) -> Vec<f32> {
    let mut values: Vec<f32> = xs.iter().zip(active).filter(|(_, a)| **a).map(|(x, _)| *x).collect();
    if values.is_empty() {
        return vec![0.0; xs.len()];
    }
    values.sort_unstable_by(f32::total_cmp);
    let (lo, hi) = (percentile_sorted(&values, NORM_LO), percentile_sorted(&values, NORM_HI));
    let span = hi - lo;
    xs.iter()
        .zip(active)
        .map(|(&x, &a)| match (a, span > 1e-6) {
            (false, _) => 0.0,
            (true, false) => 0.5, // a constant feature: neither high nor low
            (true, true) => ((x - lo) / span).clamp(0.0, 1.0),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Features {
        Features {
            fps: 25.0,
            sample_rate: 48_000,
            duration: 0.2,
            loud: vec![0.0, 0.5, 1.0, 0.25, 0.0],
            hiss: vec![0.1, 0.2, 0.3, 0.4, 0.5],
            flatness: vec![0.0; 5],
            hiband: vec![1.0; 5],
            hum: vec![0.0, 0.0, 0.0, 0.0, 0.1],
            clicks: vec![ClickEvent { time: 0.05, strength: 0.8 }],
        }
    }

    #[test]
    fn save_load_round_trip() {
        let path = std::env::temp_dir().join(format!("static-features-{}.bin", std::process::id()));
        let f = sample();
        f.save(&path).unwrap();
        let loaded = Features::load(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(loaded, f);
    }

    #[test]
    fn envelope_attacks_fast_and_releases_slowly() {
        let env = Envelope { attack: 0.0, release: 0.5 };
        let y = env.apply(&[1.0, 0.0, 0.0, 0.0], 25.0);
        assert_eq!(y[0], 1.0);
        assert!(y[1] < 1.0 && y[1] > 0.9 && y[3] < y[1]);
    }

    #[test]
    fn click_shows_in_its_own_frame() {
        let env = sample().click_envelope(0.12);
        // 0.05 s lies in frame 1 (0.04–0.08 s).
        assert_eq!(env[0], 0.0);
        assert!(env[1] > 0.7);
        assert!(env[2] < env[1] && env[3] < env[2]);
    }

    #[test]
    fn normalise_ignores_silence() {
        let n = normalise(&[-90.0, -40.0, -20.0, -30.0], &[false, true, true, true]);
        assert_eq!(n[0], 0.0);
        assert_eq!(n[1], 0.0);
        assert_eq!(n[2], 1.0);
        assert!(n[3] > 0.0 && n[3] < 1.0);
    }
}
