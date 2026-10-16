//! Turns a frame index into everything the renderer needs for that frame.
//! Grows into the timeline (cues, keyframed parameters) in milestone 6.

use crate::analysis::{FeatureFrame, Features, SmoothingParams};
use crate::look::Look;
use crate::renderer::FrameInputs;

pub struct Show {
    /// Smoothed features per frame; empty without a features file.
    frames: Vec<FeatureFrame>,
    look: Look,
}

impl Show {
    pub fn new(features: Option<&Features>, look: Look) -> Self {
        let frames = features.map(|f| f.frames(&SmoothingParams::default())).unwrap_or_default();
        Self { frames, look }
    }

    /// Number of frames in the piece, if there is audio.
    pub fn len(&self) -> Option<u32> {
        (!self.frames.is_empty()).then_some(self.frames.len() as u32)
    }

    /// Feature values at a frame; silence before/after the audio or without it.
    pub fn features(&self, index: u32) -> FeatureFrame {
        self.frames.get(index as usize).copied().unwrap_or_default()
    }

    pub fn inputs(&self, index: u32) -> FrameInputs {
        FrameInputs { index, disturbance: self.look.disturbance(&self.features(index)) }
    }
}
