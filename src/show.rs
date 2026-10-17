//! Turns a frame index into everything the renderer needs for that frame:
//! the timeline's scene, title and parameters there, plus the audio features
//! smoothed with the parameters in force at each moment.

use crate::analysis::{FeatureFrame, Features};
use crate::params::{self, P, Params};
use crate::renderer::FrameInputs;
use crate::tears::{self, Tears};
use crate::timeline::Timeline;

pub struct Show {
    timeline: Timeline,
    features: Option<Features>,
    fps: f32,
    /// Values set live in the preview; they replace the timeline's value for
    /// that parameter across the whole piece.
    overrides: [Option<f32>; params::COUNT],
    /// Smoothed features per frame; empty without features.
    frames: Vec<FeatureFrame>,
    /// Click decay per frame (the tears fade with the click envelope).
    click_decay: Vec<f32>,
}

impl Show {
    pub fn new(timeline: Timeline, features: Option<Features>, fps: f32) -> Self {
        let mut show = Self {
            timeline,
            features,
            fps,
            overrides: [None; params::COUNT],
            frames: Vec::new(),
            click_decay: Vec::new(),
        };
        show.resmooth();
        show
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    /// Swaps in a reloaded timeline, keeping features and overrides.
    pub fn set_timeline(&mut self, timeline: Timeline) {
        self.timeline = timeline;
        self.resmooth();
    }

    pub fn features(&self) -> Option<&Features> {
        self.features.as_ref()
    }

    pub fn overrides(&self) -> &[Option<f32>; params::COUNT] {
        &self.overrides
    }

    /// Sets or clears one override. Re-smooths only when it matters.
    pub fn set_override(&mut self, i: usize, value: Option<f32>) {
        if self.overrides[i] == value {
            return;
        }
        self.overrides[i] = value;
        if ["hiss", "loud", "clicks"].contains(&params::SPECS[i].group) {
            self.resmooth();
        }
    }

    pub fn clear_overrides(&mut self) {
        self.overrides = [None; params::COUNT];
        self.resmooth();
    }

    /// Every parameter at a time, overrides applied.
    pub fn params_at(&self, secs: f64) -> Params {
        let mut p = self.timeline.params_at(secs);
        for (v, o) in p.0.iter_mut().zip(&self.overrides) {
            if let Some(o) = o {
                *v = *o;
            }
        }
        p
    }

    /// Number of frames in the video (audio plus outro tail), if known.
    pub fn len(&self) -> Option<u32> {
        let secs = self.duration()?;
        Some((secs * self.fps as f64).ceil() as u32)
    }

    /// Length of the video in seconds.
    pub fn duration(&self) -> Option<f64> {
        self.timeline.duration().or_else(|| self.features.as_ref().map(|f| f.duration as f64))
    }

    /// Feature values at a frame; silence before/after the audio or without it.
    pub fn features_at(&self, index: u32) -> FeatureFrame {
        self.frames.get(index as usize).copied().unwrap_or_default()
    }

    pub fn inputs(&self, index: u32) -> FrameInputs {
        let secs = index as f64 / self.fps as f64;
        let p = self.params_at(secs);
        let scene = self.timeline.scene_at(secs);
        // A dead screen or a frozen photo isn't disturbed by the signal.
        let gain = scene.gain();
        let mut disturbance = p.look().disturbance(&self.features_at(index));
        disturbance.snow *= gain;
        disturbance.jitter *= gain;
        disturbance.tear *= gain;
        disturbance.chroma = (disturbance.chroma * p[P::ChromaSplit] * gain).min(1.0);
        let mut tears = match (&self.features, self.click_decay.get(index as usize)) {
            (Some(f), Some(&decay)) => tears::at(&f.clicks, index, f.fps, decay),
            _ => Tears::default(),
        };
        tears.roll *= gain;
        FrameInputs {
            index,
            disturbance,
            tears,
            scene,
            title: self.timeline.title_at(secs),
            react: p.react(),
            crt: p.crt(),
            grade: p.grade(),
            snow_rows: p[P::SnowRows],
        }
    }

    /// Re-smooths every frame with the parameters in force at that frame
    /// (cheap: milliseconds even for a long piece).
    fn resmooth(&mut self) {
        let Some(f) = &self.features else {
            self.frames.clear();
            self.click_decay.clear();
            return;
        };
        let fps = f.fps;
        let n = f.frame_count();
        let params: Vec<Params> = (0..n).map(|i| self.params_at(i as f64 / fps as f64)).collect();

        let follow = |xs: &[f32], attack: P, release: P, gain: P, curve: P| -> Vec<f32> {
            let coeff = |secs: f32| if secs > 0.0 { (-1.0 / (secs * fps)).exp() } else { 0.0 };
            let mut y = 0.0f32;
            xs.iter()
                .zip(&params)
                .map(|(&x, p)| {
                    let c = if x > y { coeff(p[attack]) } else { coeff(p[release]) };
                    y = x + c * (y - x);
                    (y * p[gain]).clamp(0.0, 1.0).powf(p[curve])
                })
                .collect()
        };
        let hiss = follow(&f.hiss, P::HissAttack, P::HissRelease, P::HissGain, P::HissCurve);
        let loud = follow(&f.loud, P::LoudAttack, P::LoudRelease, P::LoudGain, P::LoudCurve);

        // Click envelope: instant attack, exponential decay; a click anywhere
        // in a frame's interval shows in that frame.
        self.click_decay = params.iter().map(|p| p[P::ClickDecay].max(1e-3)).collect();
        let mut click = Vec::with_capacity(n);
        let (mut next, mut env) = (0, 0.0f32);
        for (i, p) in params.iter().enumerate() {
            let decay = self.click_decay[i];
            env *= (-1.0 / (decay * fps)).exp();
            let (mid, end) = ((i as f32 + 0.5) / fps, (i + 1) as f32 / fps);
            while next < f.clicks.len() && f.clicks[next].time < end {
                let c = f.clicks[next];
                env = env.max(c.strength * (-(mid - c.time).max(0.0) / decay).exp());
                next += 1;
            }
            click.push((env * p[P::ClickGain]).min(1.0));
        }

        self.frames =
            (0..n).map(|i| FeatureFrame { hiss: hiss[i], click: click[i], loud: loud[i], hum: 0.0 }).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Features, SmoothingParams};
    use crate::timeline::TimelineFile;

    fn features() -> Features {
        let n = 100;
        let ramp: Vec<f32> = (0..n).map(|i| if i >= 50 { 1.0 } else { 0.0 }).collect();
        Features {
            fps: 25.0,
            sample_rate: 48_000,
            duration: 4.0,
            loud: ramp.clone(),
            hiss: ramp.clone(),
            flatness: ramp.clone(),
            hiband: ramp.clone(),
            hum: vec![0.0; n],
            clicks: vec![crate::analysis::ClickEvent { time: 1.0, strength: 0.8 }],
        }
    }

    #[test]
    fn default_smoothing_matches_the_analysis_envelopes() {
        let f = features();
        let expected = f.frames(&SmoothingParams::default());
        let file =
            TimelineFile::parse("[intro]\nscene = 'photo'\n[outro]\ntail = 0", std::path::Path::new("")).unwrap();
        let show = Show::new(Timeline::compile(&file, Some(4.0)), Some(f), 25.0);
        for (i, e) in expected.iter().enumerate() {
            let got = show.features_at(i as u32);
            assert!((got.hiss - e.hiss).abs() < 1e-5 && (got.loud - e.loud).abs() < 1e-5, "frame {i}");
            assert!((got.click - e.click).abs() < 1e-5, "frame {i}: {} vs {}", got.click, e.click);
        }
    }

    #[test]
    fn overrides_win_and_gain_scales() {
        let file = TimelineFile::parse("", std::path::Path::new("")).unwrap();
        let mut show = Show::new(Timeline::compile(&file, Some(4.0)), Some(features()), 25.0);
        show.set_override(P::LoudGain as usize, Some(0.5));
        show.set_override(P::LoudAttack as usize, Some(0.0));
        assert_eq!(show.features_at(60).loud, 0.5);
        assert_eq!(show.params_at(1.0)[P::LoudGain], 0.5);
        show.clear_overrides();
        assert_eq!(show.params_at(1.0)[P::LoudGain], 1.0);
    }
}
