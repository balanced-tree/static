//! Turns a frame index into everything the renderer needs for that frame.
//! Grows into the timeline (cues, keyframed parameters) in milestone 6.

use crate::analysis::{FeatureFrame, Features, SmoothingParams};
use crate::look::Look;
use crate::renderer::FrameInputs;

pub struct Show {
    features: Option<Features>,
    smoothing: SmoothingParams,
    /// Smoothed features per frame; empty without features.
    frames: Vec<FeatureFrame>,
    pub look: Look,
}

impl Show {
    pub fn new(features: Option<Features>, look: Look) -> Self {
        let mut show = Self { features, smoothing: SmoothingParams::default(), frames: Vec::new(), look };
        show.set_smoothing(show.smoothing);
        show
    }

    pub fn features(&self) -> Option<&Features> {
        self.features.as_ref()
    }

    pub fn smoothing(&self) -> SmoothingParams {
        self.smoothing
    }

    /// Re-smooths every frame (cheap: a few ms even for a long piece).
    pub fn set_smoothing(&mut self, smoothing: SmoothingParams) {
        self.smoothing = smoothing;
        self.frames = self.features.as_ref().map(|f| f.frames(&smoothing)).unwrap_or_default();
    }

    /// Number of frames in the piece, if there is audio.
    pub fn len(&self) -> Option<u32> {
        (!self.frames.is_empty()).then_some(self.frames.len() as u32)
    }

    /// Length of the analysed audio in seconds.
    pub fn duration(&self) -> Option<f64> {
        self.features.as_ref().map(|f| f.duration as f64)
    }

    /// Feature values at a frame; silence before/after the audio or without it.
    pub fn features_at(&self, index: u32) -> FeatureFrame {
        self.frames.get(index as usize).copied().unwrap_or_default()
    }

    pub fn inputs(&self, index: u32) -> FrameInputs {
        FrameInputs { index, disturbance: self.look.disturbance(&self.features_at(index)) }
    }
}
