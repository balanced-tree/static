//! How audio features become visual disturbance: the per-layer feature
//! weights (PLAN §5 "Disturbance mix") and the stillness mapping.
//!
//! Everything here is CPU-side and per frame; the shaders only receive the
//! final amounts. Values become keyframeable in `timeline.toml` (milestone 6).

use crate::analysis::FeatureFrame;

/// How much each feature contributes to one disturbance layer.
#[derive(Clone, Copy, Debug)]
pub struct Weights {
    pub hiss: f32,
    pub click: f32,
    pub loud: f32,
}

impl Weights {
    fn mix(&self, f: &FeatureFrame) -> f32 {
        (self.hiss * f.hiss + self.click * f.click + self.loud * f.loud).clamp(0.0, 1.0)
    }
}

/// The three readings of "stillness vs restlessness" from the interview.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum StillnessMode {
    /// Silence = untouched photo; energy tears it into static.
    StillAtRest,
    /// A floor of static is always there; energy pushes it further.
    AlwaysRestless,
    /// Static is the resting state; the object is revealed by its own signal.
    Inverted,
}

#[derive(Clone, Copy, Debug)]
pub struct Stillness {
    /// Disturbance present even at zero signal (0..1).
    pub baseline: f32,
    /// Curve applied to the mixed amount: >1 keeps quiet passages calmer,
    /// <1 makes small signals visible sooner.
    pub response: f32,
    /// Flip the result: more signal = less disturbance.
    pub invert: bool,
}

impl Stillness {
    pub fn preset(mode: StillnessMode) -> Self {
        match mode {
            StillnessMode::StillAtRest => Self { baseline: 0.0, response: 1.3, invert: false },
            StillnessMode::AlwaysRestless => Self { baseline: 0.25, response: 1.0, invert: false },
            StillnessMode::Inverted => Self { baseline: 0.0, response: 0.8, invert: true },
        }
    }

    fn map(&self, amount: f32) -> f32 {
        let v = self.baseline + (1.0 - self.baseline) * self.shape(amount);
        if self.invert { 1.0 - v } else { v }
    }

    /// Just the response curve. Tears and chroma split are events, not a
    /// resting state, so they skip `baseline` and `invert`: otherwise an
    /// inverted look would tear constantly through silence.
    fn shape(&self, amount: f32) -> f32 {
        amount.clamp(0.0, 1.0).powf(self.response)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Look {
    /// The preset this look started from (its values may since be tuned).
    pub preset: StillnessMode,
    pub snow: Weights,
    pub jitter: Weights,
    pub tear: Weights,
    pub chroma: Weights,
    pub stillness: Stillness,
}

impl Look {
    pub fn new(mode: StillnessMode) -> Self {
        Self {
            preset: mode,
            snow: Weights { hiss: 0.6, click: 0.1, loud: 0.5 },
            jitter: Weights { hiss: 0.4, click: 0.2, loud: 0.6 },
            tear: Weights { hiss: 0.0, click: 0.8, loud: 0.4 },
            chroma: Weights { hiss: 0.0, click: 0.6, loud: 0.3 },
            stillness: Stillness::preset(mode),
        }
    }

    pub fn disturbance(&self, f: &FeatureFrame) -> Disturbance {
        let s = &self.stillness;
        Disturbance {
            snow: s.map(self.snow.mix(f)),
            jitter: s.map(self.jitter.mix(f)),
            tear: s.shape(self.tear.mix(f)),
            chroma: s.shape(self.chroma.mix(f)),
        }
    }
}

/// Final per-frame amounts handed to the shaders, each 0..1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Disturbance {
    pub snow: f32,
    pub jitter: f32,
    pub tear: f32,
    pub chroma: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SILENT: FeatureFrame = FeatureFrame { hiss: 0.0, click: 0.0, loud: 0.0, hum: 0.0 };
    const LOUD: FeatureFrame = FeatureFrame { hiss: 1.0, click: 1.0, loud: 1.0, hum: 0.0 };

    #[test]
    fn still_at_rest_is_clean_in_silence() {
        let look = Look::new(StillnessMode::StillAtRest);
        assert_eq!(look.disturbance(&SILENT), Disturbance::default());
        let d = look.disturbance(&LOUD);
        assert_eq!((d.snow, d.jitter, d.tear), (1.0, 1.0, 1.0));
        assert!((d.chroma - 0.9f32.powf(1.3)).abs() < 1e-6, "mix 0.9 through the response curve: {}", d.chroma);
    }

    #[test]
    fn always_restless_keeps_a_floor() {
        let d = Look::new(StillnessMode::AlwaysRestless).disturbance(&SILENT);
        assert_eq!(d.snow, 0.25);
    }

    #[test]
    fn inverted_is_static_in_silence_and_clear_when_charged() {
        let look = Look::new(StillnessMode::Inverted);
        assert_eq!(look.disturbance(&SILENT).snow, 1.0);
        assert_eq!(look.disturbance(&LOUD).snow, 0.0);
        assert_eq!(look.disturbance(&SILENT).tear, 0.0, "silence never tears");
    }
}
