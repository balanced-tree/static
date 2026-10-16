//! Every keyframeable parameter, in one table. The timeline parser, the
//! keyframe interpolation, the preview's sliders and "Copy as TOML" all work
//! from this list, so adding a parameter here makes it available everywhere.
//!
//! In `timeline.toml` a parameter is written as `group = { key = value }`,
//! e.g. `hiss = { gain = 1.4, release = "800ms" }`.

use crate::look::{Look, Stillness, Weights};
use crate::renderer::{Crt, Grade};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Number,
    /// Seconds; written as `"800ms"`, `"2s"` or a bare number.
    Duration,
    /// 0 or 1; written as `true` / `false`.
    Bool,
    /// One channel of a colour written as `"#rrggbb"` on the group's key
    /// without the `.r/.g/.b` suffix (e.g. `colour = { tint = "#c8ffe0" }`).
    Channel,
}

pub struct Spec {
    pub group: &'static str,
    pub key: &'static str,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub kind: Kind,
}

const fn spec(group: &'static str, key: &'static str, default: f32, min: f32, max: f32, kind: Kind) -> Spec {
    Spec { group, key, default, min, max, kind }
}

use Kind::*;

/// Indices into `SPECS`; keep in the same order. Not every variant is named
/// in code: some are only reached through the table.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum P {
    Baseline,
    Response,
    Invert,
    SnowHiss,
    SnowClick,
    SnowLoud,
    SnowThreshold,
    JitterHiss,
    JitterClick,
    JitterLoud,
    JitterThreshold,
    TearHiss,
    TearClick,
    TearLoud,
    TearThreshold,
    ChromaHiss,
    ChromaClick,
    ChromaLoud,
    ChromaThreshold,
    HissAttack,
    HissRelease,
    HissGain,
    HissCurve,
    LoudAttack,
    LoudRelease,
    LoudGain,
    LoudCurve,
    ClickDecay,
    ClickGain,
    SnowRows,
    Scanlines,
    Vignette,
    Bloom,
    Persistence,
    Saturation,
    ChromaSplit,
    Contrast,
    BlackLevel,
    TintR,
    TintG,
    TintB,
}

pub const COUNT: usize = P::TintB as usize + 1;

pub const SPECS: [Spec; COUNT] = [
    spec("stillness", "baseline", 0.0, 0.0, 1.0, Number),
    spec("stillness", "response", 1.3, 0.25, 4.0, Number),
    spec("stillness", "invert", 0.0, 0.0, 1.0, Bool),
    spec("snow_mix", "hiss", 0.6, 0.0, 1.5, Number),
    spec("snow_mix", "click", 0.1, 0.0, 1.5, Number),
    spec("snow_mix", "loud", 0.5, 0.0, 1.5, Number),
    spec("snow_mix", "threshold", 0.0, 0.0, 0.9, Number),
    spec("jitter_mix", "hiss", 0.4, 0.0, 1.5, Number),
    spec("jitter_mix", "click", 0.2, 0.0, 1.5, Number),
    spec("jitter_mix", "loud", 0.6, 0.0, 1.5, Number),
    spec("jitter_mix", "threshold", 0.0, 0.0, 0.9, Number),
    spec("tear_mix", "hiss", 0.0, 0.0, 1.5, Number),
    spec("tear_mix", "click", 0.8, 0.0, 1.5, Number),
    spec("tear_mix", "loud", 0.4, 0.0, 1.5, Number),
    spec("tear_mix", "threshold", 0.0, 0.0, 0.9, Number),
    spec("chroma_mix", "hiss", 0.0, 0.0, 1.5, Number),
    spec("chroma_mix", "click", 0.6, 0.0, 1.5, Number),
    spec("chroma_mix", "loud", 0.3, 0.0, 1.5, Number),
    spec("chroma_mix", "threshold", 0.0, 0.0, 0.9, Number),
    spec("hiss", "attack", 0.15, 0.0, 2.0, Duration),
    spec("hiss", "release", 0.6, 0.0, 4.0, Duration),
    spec("hiss", "gain", 1.0, 0.0, 3.0, Number),
    spec("hiss", "curve", 1.0, 0.25, 4.0, Number),
    spec("loud", "attack", 0.05, 0.0, 2.0, Duration),
    spec("loud", "release", 0.3, 0.0, 4.0, Duration),
    spec("loud", "gain", 1.0, 0.0, 3.0, Number),
    spec("loud", "curve", 1.0, 0.25, 4.0, Number),
    spec("clicks", "decay", 0.12, 0.02, 1.0, Duration),
    spec("clicks", "gain", 1.0, 0.0, 3.0, Number),
    spec("snow", "rows", 540.0, 90.0, 1080.0, Number),
    spec("crt", "scanlines", 0.3, 0.0, 1.0, Number),
    spec("crt", "vignette", 0.25, 0.0, 1.0, Number),
    spec("crt", "bloom", 0.15, 0.0, 1.0, Number),
    spec("crt", "persistence", 0.0, 0.0, 0.95, Number),
    spec("colour", "saturation", 1.0, 0.0, 1.5, Number),
    spec("colour", "chroma_split", 1.0, 0.0, 3.0, Number),
    spec("colour", "contrast", 1.0, 0.5, 2.0, Number),
    spec("colour", "black_level", 0.0, 0.0, 0.3, Number),
    spec("colour", "tint.r", 1.0, 0.0, 1.0, Channel),
    spec("colour", "tint.g", 1.0, 0.0, 1.0, Channel),
    spec("colour", "tint.b", 1.0, 0.0, 1.0, Channel),
];

/// Parameter groups in display order.
pub const GROUPS: [&str; 11] = [
    "stillness",
    "snow_mix",
    "jitter_mix",
    "tear_mix",
    "chroma_mix",
    "hiss",
    "loud",
    "clicks",
    "snow",
    "crt",
    "colour",
];

/// A full set of values, one per `SPECS` entry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params(pub [f32; COUNT]);

impl Default for Params {
    fn default() -> Self {
        Self(std::array::from_fn(|i| SPECS[i].default))
    }
}

impl std::ops::Index<P> for Params {
    type Output = f32;
    fn index(&self, p: P) -> &f32 {
        &self.0[p as usize]
    }
}

impl std::ops::IndexMut<P> for Params {
    fn index_mut(&mut self, p: P) -> &mut f32 {
        &mut self.0[p as usize]
    }
}

impl Params {
    fn weights(&self, first: P) -> Weights {
        let i = first as usize;
        Weights { hiss: self.0[i], click: self.0[i + 1], loud: self.0[i + 2], threshold: self.0[i + 3] }
    }

    pub fn look(&self) -> Look {
        Look {
            snow: self.weights(P::SnowHiss),
            jitter: self.weights(P::JitterHiss),
            tear: self.weights(P::TearHiss),
            chroma: self.weights(P::ChromaHiss),
            stillness: Stillness {
                baseline: self[P::Baseline],
                response: self[P::Response],
                invert: self[P::Invert] > 0.5,
            },
        }
    }

    pub fn crt(&self) -> Crt {
        Crt {
            scanlines: self[P::Scanlines],
            vignette: self[P::Vignette],
            bloom: self[P::Bloom],
            persistence: self[P::Persistence],
        }
    }

    pub fn grade(&self) -> Grade {
        Grade {
            saturation: self[P::Saturation],
            contrast: self[P::Contrast],
            black_level: self[P::BlackLevel],
            tint: [self[P::TintR], self[P::TintG], self[P::TintB]],
        }
    }
}

/// The parameter written as `group.key` (colour channels: `colour.tint.r`).
pub fn find(group: &str, key: &str) -> Option<usize> {
    SPECS.iter().position(|s| s.group == group && s.key == key)
}

/// Indices of a group's parameters, in table order.
pub fn group(name: &str) -> impl Iterator<Item = usize> + '_ {
    (0..COUNT).filter(move |&i| SPECS[i].group == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_match_the_table() {
        assert_eq!(SPECS[P::Baseline as usize].key, "baseline");
        assert_eq!(SPECS[P::ClickGain as usize].group, "clicks");
        assert_eq!(SPECS[P::SnowRows as usize].group, "snow");
        assert_eq!(SPECS[P::TintB as usize].key, "tint.b");
        assert_eq!(SPECS[P::JitterThreshold as usize].group, "jitter_mix");
        assert_eq!(SPECS[P::ChromaThreshold as usize].key, "threshold");
        for g in GROUPS {
            assert!(group(g).next().is_some(), "empty group {g}");
        }
        assert_eq!(GROUPS.iter().map(|g| group(g).count()).sum::<usize>(), COUNT);
    }
}
