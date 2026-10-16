//! Offline audio analysis: WAV → per-video-frame features.
//!
//! Analysis runs once over the whole piece, so features can be normalised
//! against the piece itself. Smoothing is applied later, at load time (see
//! `SmoothingParams`), so it can be tuned without re-analysing.

mod decode;
mod features;
mod onsets;
mod plot;
mod spectral;

pub use decode::Audio;
pub use features::{ClickEvent, FeatureFrame, Features, SmoothingParams};
pub use plot::write_svg;

use std::path::Path;

use anyhow::Result;

pub struct AnalysisParams {
    pub fps: f32,
    /// How far above the local flux level (in robust standard deviations) a
    /// click must rise. Lower = more clicks detected.
    pub click_threshold: f32,
}

pub fn analyse(path: &Path, params: &AnalysisParams) -> Result<Features> {
    Ok(analyse_audio(&decode::read_mono(path)?, params))
}

pub fn analyse_audio(audio: &Audio, params: &AnalysisParams) -> Features {
    let measures = spectral::measure_frames(audio, params.fps);
    let (flux, step) = spectral::onset_flux(audio);
    let clicks = onsets::pick(&flux, step, params.click_threshold);
    Features::from_measures(&measures, clicks, params.fps, audio)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    /// Deterministic white noise in -1..1 (xorshift).
    struct Noise(u32);
    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
        }
    }

    fn db(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    const CLICKS: [f32; 4] = [9.0, 9.5, 10.25, 11.0];

    /// 0–2 s silence · 2–4 s tone · 4–6 s quiet noise · 6–8 s loud noise ·
    /// 8–12 s faint noise floor with clicks at `CLICKS`.
    fn test_signal() -> Audio {
        let mut noise = Noise(0x1234_5678);
        let samples = (0..SR as usize * 12)
            .map(|i| {
                let t = i as f32 / SR as f32;
                match t {
                    t if t < 2.0 => 0.0,
                    t if t < 4.0 => db(-12.0) * (std::f32::consts::TAU * 440.0 * t).sin(),
                    t if t < 6.0 => db(-30.0) * noise.next(),
                    t if t < 8.0 => db(-10.0) * noise.next(),
                    _ => {
                        let click = CLICKS.iter().any(|&c| (t - c).abs() < 0.001);
                        db(-60.0) * noise.next() + if click { 0.8 } else { 0.0 }
                    }
                }
            })
            .collect();
        Audio { samples, sample_rate: SR }
    }

    fn mean_over(series: &[f32], fps: f32, from: f32, to: f32) -> f32 {
        // Skip the edges of each section, where windows straddle the boundary.
        let (a, b) = (((from + 0.2) * fps) as usize, ((to - 0.2) * fps) as usize);
        series[a..b].iter().sum::<f32>() / (b - a) as f32
    }

    #[test]
    fn features_follow_the_test_signal() {
        let fps = 25.0;
        let f = analyse_audio(&test_signal(), &AnalysisParams { fps, click_threshold: 4.0 });
        assert_eq!(f.frame_count(), 300);

        // Digital silence reads as nothing at all.
        assert_eq!(mean_over(&f.loud, fps, 0.0, 2.0), 0.0);
        assert_eq!(mean_over(&f.hiss, fps, 0.0, 2.0), 0.0);

        // Hiss: noise ≫ tone, and louder noise > quieter noise.
        let (tone, quiet, loud) =
            (mean_over(&f.hiss, fps, 2.0, 4.0), mean_over(&f.hiss, fps, 4.0, 6.0), mean_over(&f.hiss, fps, 6.0, 8.0));
        assert!(tone < 0.1, "tone hiss {tone}");
        assert!(quiet > tone + 0.4, "quiet noise hiss {quiet}");
        assert!(loud > quiet, "loud {loud} vs quiet {quiet}");

        // Loudness ordering: -10 dB noise > -12 dB tone > -30 dB noise.
        let l = |a, b| mean_over(&f.loud, fps, a, b);
        assert!(l(6.0, 8.0) > l(2.0, 4.0) && l(2.0, 4.0) > l(4.0, 6.0));

        // Every click found within ±25 ms, and nothing else in that section.
        let found: Vec<f32> = f.clicks.iter().map(|c| c.time).filter(|&t| t > 8.2).collect();
        assert_eq!(found.len(), CLICKS.len(), "found {found:?}");
        for (got, want) in found.iter().zip(CLICKS) {
            assert!((got - want).abs() < 0.025, "click at {got}, expected {want}");
        }
    }

    #[test]
    fn click_envelope_peaks_on_the_click_frame() {
        let fps = 25.0;
        let f = analyse_audio(&test_signal(), &AnalysisParams { fps, click_threshold: 4.0 });
        let frames = f.frames(&SmoothingParams::default());
        let at = |t: f32| frames[(t * fps) as usize].click;
        assert!(at(9.0) > 0.5, "click frame {}", at(9.0));
        assert!(at(9.0) > at(9.2) && at(9.2) > at(9.4), "decays after the click");
        assert!(at(8.6) < 0.01, "quiet before the click");
    }
}
