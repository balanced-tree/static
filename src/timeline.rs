//! `timeline.toml` (PLAN §7): cues, `[intro]` / `[outro]`, `[title]` and
//! keyframed parameters.
//!
//! Parsing gives a `TimelineFile`, which is compiled against the audio's
//! length into a `Timeline`; that answers, for any time, which scene(s) are
//! showing, every parameter's value and the title's visibility. All of it is
//! a pure function of time, so any frame can be rendered on its own.
//!
//! - A cue's **scene** (`photo`, or `scene = "static" | "black"`) changes at
//!   `at` and takes `crossfade` to arrive, using `transition`.
//! - A cue's **parameters** start moving at `at` and reach their new values
//!   after `ramp` (default 4 s; `"next"` = drift until the next cue), shaped
//!   by `ease`. Parameters a cue doesn't mention keep their values.
//! - `[intro]` and `[outro]` are cues at the start and end, with their own
//!   timing keys. Both default to static when absent.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use toml::{Table, Value};

use crate::assets::{Fit, Framing};
use crate::look::Stillness;
use crate::params::{self, Kind, P, Params, SPECS};

const DEFAULT_RAMP: f64 = 4.0;
const DEFAULT_CROSSFADE: f64 = 6.0;
/// How long `end_on` holds the final scene at the very end of the video.
const END_HOLD: f64 = 1.0;

// ---------------------------------------------------------------------------
// Scenes

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum SceneKind {
    Static = 0,
    Black = 1,
    Photo = 2,
}

/// What one scene layer shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scene {
    pub kind: SceneKind,
    /// Photo layer (index into `Timeline::photos`).
    pub layer: u32,
    pub soften: f32,
    /// How much the signal disturbs this scene: 1 normally, 0 for a dead
    /// screen (`black`) or a perfectly still `freeze`.
    pub gain: f32,
}

impl Scene {
    pub const STATIC: Self = Self { kind: SceneKind::Static, layer: 0, soften: 0.0, gain: 1.0 };
    pub const BLACK: Self = Self { kind: SceneKind::Black, layer: 0, soften: 0.0, gain: 0.0 };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Transition {
    Cut = 0,
    Dissolve = 1,
    /// The new scene arrives cell by cell through a band of static.
    #[default]
    NoiseDissolve = 2,
}

/// The scene(s) on screen at one moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneMix {
    pub from: Scene,
    pub to: Scene,
    /// 0 = all `from`, 1 = all `to`. Already eased for `Dissolve`.
    pub progress: f32,
    pub transition: Transition,
    /// Differs per transition, so no two dissolves share a pattern.
    pub seed: u32,
}

impl SceneMix {
    pub fn steady(scene: Scene) -> Self {
        Self { from: scene, to: scene, progress: 1.0, transition: Transition::Cut, seed: 0 }
    }

    /// Disturbance gain across the transition.
    pub fn gain(&self) -> f32 {
        self.from.gain + (self.to.gain - self.from.gain) * self.progress
    }
}

/// A photo as one cue frames it; each distinct one becomes a texture layer.
#[derive(Clone, Debug, PartialEq)]
pub struct PhotoSpec {
    pub path: PathBuf,
    pub framing: Framing,
    /// 0..1: blur plus stronger grain, for photos upscaled a long way.
    pub soften: f32,
}

#[derive(Clone, Debug, PartialEq)]
enum SceneSpec {
    Photo(PhotoSpec),
    Static,
    Black,
}

// ---------------------------------------------------------------------------
// Parsed file

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ramp {
    Secs(f64),
    Next,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ease {
    Linear,
    Smooth,
    Step,
}

impl Ease {
    fn apply(self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Ease::Linear => x,
            Ease::Smooth => x * x * (3.0 - 2.0 * x),
            Ease::Step => 1.0,
        }
    }
}

/// Parameter values set by a cue (index into `params::SPECS`, value).
type Settings = Vec<(usize, f32)>;

#[derive(Clone, Debug)]
struct Cue {
    at: f64,
    scene: Option<SceneSpec>,
    crossfade: f64,
    transition: Transition,
    ramp: Ramp,
    ease: Ease,
    params: Settings,
}

#[derive(Clone, Debug)]
pub struct Intro {
    /// `None` = start on the first cue's photo.
    scene: Option<SceneSpec>,
    hold: f64,
    resolve: f64,
    transition: Transition,
    params: Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutroScene {
    Static,
    Black,
    /// Hold the last photo, perfectly still.
    Freeze,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndOn {
    Black,
    Static,
    Cut,
}

#[derive(Clone, Debug)]
pub struct Outro {
    scene: OutroScene,
    /// Seconds relative to the end of the audio (negative = before).
    begin: f64,
    crossfade: f64,
    transition: Transition,
    tail: f64,
    end_on: EndOn,
    params: Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reveal {
    Noise,
    Fade,
    Cut,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TitleSpec {
    pub text: String,
    pub subtitle: String,
    pub font: PathBuf,
    /// Cap heights as fractions of the frame height.
    pub size: f32,
    pub subtitle_size: f32,
    pub position: [f32; 2],
    pub align: Align,
    /// Linear RGB.
    pub colour: [f32; 3],
    /// In em.
    pub letter_spacing: f32,
    appear: f64,
    reveal: Reveal,
    reveal_time: f64,
    hold: f64,
    vanish: Reveal,
    vanish_time: f64,
    pub disturb: f32,
}

/// The title at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TitleState {
    /// 0 = hidden, 1 = fully shown.
    pub alpha: f32,
    /// Appearing or vanishing cell by cell through the static, not fading.
    pub noise: bool,
    pub disturb: f32,
    /// Linear RGB.
    pub colour: [f32; 3],
}

/// `timeline.toml` as written, before it is placed against the audio.
#[derive(Clone, Debug)]
pub struct TimelineFile {
    pub audio: Option<PathBuf>,
    pub features: Option<PathBuf>,
    pub seed: u32,
    intro: Option<Intro>,
    outro: Option<Outro>,
    pub title: Option<TitleSpec>,
    cues: Vec<Cue>,
}

impl TimelineFile {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text, path.parent().unwrap_or(Path::new(""))).with_context(|| format!("in {}", path.display()))
    }

    /// Paths in the file are taken relative to `dir`.
    pub fn parse(text: &str, dir: &Path) -> Result<Self> {
        let mut doc: Table = text.parse().context("not valid TOML")?;
        let path = |p: &str| dir.join(p);

        let mut file = TimelineFile {
            audio: None,
            features: None,
            seed: 1234,
            intro: Some(Intro {
                scene: Some(SceneSpec::Static),
                hold: 6.0,
                resolve: 8.0,
                transition: Transition::NoiseDissolve,
                params: Vec::new(),
            }),
            outro: Some(Outro {
                scene: OutroScene::Static,
                begin: -10.0,
                crossfade: DEFAULT_CROSSFADE,
                transition: Transition::NoiseDissolve,
                tail: 3.0,
                end_on: EndOn::Black,
                params: Vec::new(),
            }),
            title: None,
            cues: Vec::new(),
        };

        if let Some(meta) = doc.remove("meta") {
            let mut meta = Section::new("[meta]", meta)?;
            file.audio = meta.string("audio")?.map(|s| path(&s));
            file.features = meta.string("features")?.map(|s| path(&s));
            if let Some(seed) = meta.integer("seed")? {
                file.seed = u32::try_from(seed).context("[meta] seed must be 0..2^32")?;
            }
            if let Some(fps) = meta.number("fps")? {
                ensure!(fps == crate::FPS as f64, "[meta] fps: only {} fps is supported", crate::FPS);
            }
            meta.finish()?;
        }

        if let Some(intro) = doc.remove("intro") {
            let mut s = Section::new("[intro]", intro)?;
            let scene = match s.string("scene")?.as_deref() {
                None | Some("static") => Some(SceneSpec::Static),
                Some("black") => Some(SceneSpec::Black),
                Some("photo") => None,
                Some(other) => bail!("[intro] scene: expected \"static\", \"black\" or \"photo\", got {other:?}"),
            };
            let intro = Intro {
                scene,
                hold: s.time("hold")?.unwrap_or(6.0),
                resolve: s.time("resolve")?.unwrap_or(8.0),
                transition: s.transition()?.unwrap_or_default(),
                params: s.params()?,
            };
            s.finish()?;
            file.intro = Some(intro);
        }

        if let Some(outro) = doc.remove("outro") {
            let mut s = Section::new("[outro]", outro)?;
            let scene = match s.string("scene")?.as_deref() {
                None | Some("static") => OutroScene::Static,
                Some("black") => OutroScene::Black,
                Some("freeze") => OutroScene::Freeze,
                Some(other) => bail!("[outro] scene: expected \"static\", \"black\" or \"freeze\", got {other:?}"),
            };
            let end_on = match s.string("end_on")?.as_deref() {
                None | Some("black") => EndOn::Black,
                Some("static") => EndOn::Static,
                Some("cut") => EndOn::Cut,
                Some(other) => bail!("[outro] end_on: expected \"black\", \"static\" or \"cut\", got {other:?}"),
            };
            let outro = Outro {
                scene,
                begin: s.time("begin")?.unwrap_or(-10.0),
                crossfade: s.time("crossfade")?.unwrap_or(DEFAULT_CROSSFADE),
                transition: s.transition()?.unwrap_or_default(),
                tail: s.time("tail")?.unwrap_or(3.0),
                end_on,
                params: s.params()?,
            };
            ensure!(outro.tail >= 0.0, "[outro] tail must not be negative");
            s.finish()?;
            file.outro = Some(outro);
        }

        if let Some(title) = doc.remove("title") {
            let mut s = Section::new("[title]", title)?;
            let reveal = |s: &mut Section, key: &str| -> Result<Reveal> {
                Ok(match s.string(key)?.as_deref() {
                    None | Some("noise") => Reveal::Noise,
                    Some("fade") => Reveal::Fade,
                    Some("cut") => Reveal::Cut,
                    Some(other) => bail!("[title] {key}: expected \"noise\", \"fade\" or \"cut\", got {other:?}"),
                })
            };
            let title = TitleSpec {
                text: s.string("text")?.context("[title] needs `text`")?,
                subtitle: s.string("subtitle")?.unwrap_or_default(),
                font: path(&s.string("font")?.unwrap_or_else(|| "fonts/title.ttf".into())),
                size: s.number("size")?.unwrap_or(0.045) as f32,
                subtitle_size: s.number("subtitle_size")?.unwrap_or(0.022) as f32,
                position: s.pair("position")?.unwrap_or([0.5, 0.5]),
                align: match s.string("align")?.as_deref() {
                    None | Some("center") | Some("centre") => Align::Center,
                    Some("left") => Align::Left,
                    Some("right") => Align::Right,
                    Some(other) => bail!("[title] align: expected \"left\", \"center\" or \"right\", got {other:?}"),
                },
                colour: s.colour("colour")?.map_or([0.79; 3], srgb_to_linear),
                letter_spacing: s.number("letter_spacing")?.unwrap_or(0.08) as f32,
                appear: s.time("appear")?.unwrap_or(3.0),
                reveal: reveal(&mut s, "reveal")?,
                reveal_time: s.time("reveal_time")?.unwrap_or(2.5),
                hold: s.time("hold")?.unwrap_or(6.0),
                vanish: reveal(&mut s, "vanish")?,
                vanish_time: s.time("vanish_time")?.unwrap_or(3.0),
                disturb: s.number("disturb")?.unwrap_or(0.5).clamp(0.0, 1.0) as f32,
            };
            s.finish()?;
            file.title = Some(title);
        }

        if let Some(cues) = doc.remove("cue") {
            let Value::Array(cues) = cues else { bail!("cues are written as [[cue]] tables") };
            for (i, cue) in cues.into_iter().enumerate() {
                let label = format!("cue {} ([[cue]] #{})", i + 1, i + 1);
                let mut s = Section::new(&label, cue)?;
                let at = s.time("at")?.with_context(|| format!("{label} needs `at`"))?;
                let photo = s.string("photo")?;
                let scene = match (photo, s.string("scene")?.as_deref()) {
                    (Some(_), Some(_)) => bail!("{label}: give either `photo` or `scene`, not both"),
                    (Some(p), None) => Some(SceneSpec::Photo(PhotoSpec {
                        path: path(&p),
                        framing: Framing {
                            fit: match s.string("fit")?.as_deref() {
                                None | Some("fill") => Fit::Fill,
                                Some("contain") => Fit::Contain,
                                Some(other) => bail!("{label} fit: expected \"fill\" or \"contain\", got {other:?}"),
                            },
                            focus: s.pair("focus")?.unwrap_or([0.5, 0.5]),
                        },
                        soften: s.number("soften")?.unwrap_or(0.0).clamp(0.0, 1.0) as f32,
                    })),
                    (None, Some("static")) => Some(SceneSpec::Static),
                    (None, Some("black")) => Some(SceneSpec::Black),
                    (None, Some(other)) => bail!("{label} scene: expected \"static\" or \"black\", got {other:?}"),
                    (None, None) => None,
                };
                let ramp = match s.take("ramp") {
                    Some(Value::String(r)) if r == "next" => Ramp::Next,
                    Some(v) => Ramp::Secs(time_value(&v).with_context(|| format!("{label} ramp"))?),
                    None => Ramp::Secs(DEFAULT_RAMP),
                };
                let ease = match s.string("ease")?.as_deref() {
                    None | Some("smooth") => Ease::Smooth,
                    Some("linear") => Ease::Linear,
                    Some("step") => Ease::Step,
                    Some(other) => bail!("{label} ease: expected \"smooth\", \"linear\" or \"step\", got {other:?}"),
                };
                let cue = Cue {
                    at,
                    scene,
                    crossfade: s.time("crossfade")?.unwrap_or(DEFAULT_CROSSFADE),
                    transition: s.transition()?.unwrap_or_default(),
                    ramp,
                    ease,
                    params: s.params()?,
                };
                s.finish()?;
                file.cues.push(cue);
            }
            // Stable: cues at the same time apply in file order.
            file.cues.sort_by(|a, b| a.at.total_cmp(&b.at));
        }

        if let Some(key) = doc.keys().next() {
            bail!("unknown section [{key}] (expected meta, intro, outro, title or [[cue]])");
        }
        Ok(file)
    }

    /// A timeline for running without a file: one photo (or the test card,
    /// as layer 0 with no path), no intro, outro or title.
    pub fn single(photo: Option<PhotoSpec>, stillness: Stillness, seed: u32) -> Self {
        let scene = photo.map_or(
            SceneSpec::Photo(PhotoSpec {
                path: PathBuf::new(),
                framing: Framing { fit: Fit::Fill, focus: [0.5, 0.5] },
                soften: 0.0,
            }),
            SceneSpec::Photo,
        );
        let params = vec![
            (P::Baseline as usize, stillness.baseline),
            (P::Response as usize, stillness.response),
            (P::Invert as usize, if stillness.invert { 1.0 } else { 0.0 }),
        ];
        TimelineFile {
            audio: None,
            features: None,
            seed,
            intro: None,
            outro: None,
            title: None,
            cues: vec![Cue {
                at: 0.0,
                scene: Some(scene),
                crossfade: 0.0,
                transition: Transition::Cut,
                ramp: Ramp::Secs(0.0),
                ease: Ease::Step,
                params,
            }],
        }
    }
}

// ---------------------------------------------------------------------------
// Compiled timeline

#[derive(Clone, Copy, Debug)]
struct Key {
    start: f64,
    ramp: f64,
    ease: Ease,
    value: f32,
}

#[derive(Clone, Copy, Debug)]
struct SceneEvent {
    start: f64,
    duration: f64,
    transition: Transition,
    scene: Scene,
}

/// A labelled point on the timeline, for the preview's scrub strip.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub time: f64,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Timeline {
    /// Texture layers, in order.
    pub photos: Vec<PhotoSpec>,
    pub title: Option<TitleSpec>,
    pub markers: Vec<Marker>,
    /// Video length including the outro tail; `None` without audio.
    duration: Option<f64>,
    scenes: Vec<SceneEvent>,
    /// Keyframes per parameter, sorted by start.
    keys: Vec<Vec<Key>>,
}

impl Timeline {
    /// Places the file against audio `audio_secs` long (the outro and
    /// `ramp = "next"` on the last cue need it).
    pub fn compile(file: &TimelineFile, audio_secs: Option<f64>) -> Self {
        let mut photos: Vec<PhotoSpec> = Vec::new();
        let mut scene_of = |spec: &SceneSpec| match spec {
            SceneSpec::Static => Scene::STATIC,
            SceneSpec::Black => Scene::BLACK,
            SceneSpec::Photo(p) => {
                let layer = photos.iter().position(|q| q == p).unwrap_or_else(|| {
                    photos.push(p.clone());
                    photos.len() - 1
                });
                Scene { kind: SceneKind::Photo, layer: layer as u32, soften: p.soften, gain: 1.0 }
            }
        };

        let outro = file.outro.as_ref().zip(audio_secs);
        let outro_start = outro.map(|(o, secs)| (secs + o.begin).max(0.0));
        let duration = audio_secs.map(|secs| secs + outro.map_or(0.0, |(o, _)| o.tail));

        let mut scenes = Vec::new();
        let mut markers = Vec::new();
        let mut keys: Vec<Vec<Key>> = vec![Vec::new(); params::COUNT];
        let mut add_keys = |settings: &Settings, start: f64, ramp: f64, ease: Ease| {
            for &(i, value) in settings {
                keys[i].push(Key { start, ramp, ease, value });
            }
        };

        // Intro: its scene from 0; the first cue's scene arrives at `hold`.
        let first_scene_cue = file.cues.iter().position(|c| c.scene.is_some());
        let mut first_scene_handled = false;
        if let Some(intro) = &file.intro {
            add_keys(&intro.params, 0.0, 0.0, Ease::Step);
            match &intro.scene {
                Some(spec) => {
                    let scene = scene_of(spec);
                    scenes.push(SceneEvent { start: 0.0, duration: 0.0, transition: Transition::Cut, scene });
                    if let Some(i) = first_scene_cue {
                        let target = scene_of(file.cues[i].scene.as_ref().unwrap());
                        scenes.push(SceneEvent {
                            start: intro.hold,
                            duration: intro.resolve,
                            transition: intro.transition,
                            scene: target,
                        });
                        first_scene_handled = true;
                    }
                    markers.push(Marker { time: intro.hold, label: "intro resolves".into() });
                }
                None => {
                    // "photo": the first cue's photo is up from the start.
                    if let Some(i) = first_scene_cue {
                        let scene = scene_of(file.cues[i].scene.as_ref().unwrap());
                        scenes.push(SceneEvent { start: 0.0, duration: 0.0, transition: Transition::Cut, scene });
                        first_scene_handled = true;
                    }
                }
            }
        }
        if scenes.is_empty() {
            scenes.push(SceneEvent { start: 0.0, duration: 0.0, transition: Transition::Cut, scene: Scene::BLACK });
        }

        for (i, cue) in file.cues.iter().enumerate() {
            markers.push(Marker { time: cue.at, label: format!("cue {}", i + 1) });
            if let Some(spec) = &cue.scene
                && !(first_scene_handled && Some(i) == first_scene_cue)
            {
                let scene = scene_of(spec);
                let (duration, transition) =
                    if cue.crossfade > 0.0 { (cue.crossfade, cue.transition) } else { (0.0, Transition::Cut) };
                scenes.push(SceneEvent { start: cue.at, duration, transition, scene });
            }
            let ramp = match cue.ramp {
                Ramp::Secs(s) => s,
                Ramp::Next => {
                    let next = file.cues.get(i + 1).map(|c| c.at).or(outro_start).or(duration);
                    next.map_or(0.0, |n| (n - cue.at).max(0.0))
                }
            };
            add_keys(&cue.params, cue.at, if cue.ease == Ease::Step { 0.0 } else { ramp }, cue.ease);
        }

        if let (Some((outro, _)), Some(start), Some(end)) = (outro, outro_start, duration) {
            add_keys(&outro.params, start, outro.crossfade, Ease::Smooth);
            let scene = match outro.scene {
                OutroScene::Static => Scene::STATIC,
                OutroScene::Black => Scene::BLACK,
                OutroScene::Freeze => {
                    // The photo showing when the outro begins, now perfectly still.
                    let shown = scenes.iter().rev().find(|e| e.start <= start && e.scene.kind == SceneKind::Photo);
                    shown.map_or(Scene::BLACK, |e| Scene { gain: 0.0, ..e.scene })
                }
            };
            scenes.push(SceneEvent { start, duration: outro.crossfade, transition: outro.transition, scene });
            markers.push(Marker { time: start, label: "outro".into() });
            let end_scene = match outro.end_on {
                EndOn::Black => Some(Scene::BLACK),
                EndOn::Static => Some(Scene::STATIC),
                EndOn::Cut => None,
            };
            if let Some(scene) = end_scene {
                let start = (end - END_HOLD).max(start);
                scenes.push(SceneEvent { start, duration: 0.0, transition: Transition::Cut, scene });
            }
        }

        scenes.sort_by(|a, b| a.start.total_cmp(&b.start));
        for k in &mut keys {
            k.sort_by(|a, b| a.start.total_cmp(&b.start));
        }
        markers.sort_by(|a, b| a.time.total_cmp(&b.time));
        Timeline { photos, title: file.title.clone(), markers, duration, scenes, keys }
    }

    /// Video length in seconds, including the outro tail.
    pub fn duration(&self) -> Option<f64> {
        self.duration
    }

    pub fn param_at(&self, i: usize, t: f64) -> f32 {
        let mut v = SPECS[i].default;
        let keys = &self.keys[i];
        for (k, key) in keys.iter().enumerate() {
            if key.start > t {
                break;
            }
            // Follow this key until the next one takes over (or until `t`).
            let until = keys.get(k + 1).map_or(t, |next| next.start.min(t));
            let x = if key.ramp > 0.0 { (until - key.start) / key.ramp } else { 1.0 };
            v += (key.value - v) * key.ease.apply(x) as f32;
        }
        v
    }

    pub fn params_at(&self, t: f64) -> Params {
        Params(std::array::from_fn(|i| self.param_at(i, t)))
    }

    pub fn scene_at(&self, t: f64) -> SceneMix {
        let i = self.scenes.partition_point(|e| e.start <= t).saturating_sub(1);
        let event = self.scenes[i];
        let from = if i == 0 { event.scene } else { self.scenes[i - 1].scene };
        let x = if event.duration > 0.0 { ((t - event.start) / event.duration).clamp(0.0, 1.0) } else { 1.0 };
        if x >= 1.0 || from == event.scene {
            return SceneMix::steady(event.scene);
        }
        let progress = match event.transition {
            Transition::Cut => 1.0,
            Transition::Dissolve => Ease::Smooth.apply(x),
            Transition::NoiseDissolve => x,
        } as f32;
        SceneMix { from, to: event.scene, progress, transition: event.transition, seed: i as u32 }
    }

    pub fn title_at(&self, t: f64) -> TitleState {
        let Some(title) = &self.title else { return TitleState::default() };
        let shown = title.appear + title.reveal_time;
        let gone = shown + title.hold + title.vanish_time;
        let ramp = |x: f64, reveal: Reveal| match reveal {
            Reveal::Cut => 1.0,
            Reveal::Fade => Ease::Smooth.apply(x),
            Reveal::Noise => x.clamp(0.0, 1.0),
        };
        let (alpha, mode) = if t < title.appear || t >= gone {
            (0.0, Reveal::Cut)
        } else if t < shown {
            (ramp((t - title.appear) / title.reveal_time.max(1e-6), title.reveal), title.reveal)
        } else if t < shown + title.hold {
            (1.0, Reveal::Cut)
        } else {
            let x = (t - shown - title.hold) / title.vanish_time.max(1e-6);
            (1.0 - ramp(x, title.vanish), title.vanish)
        };
        TitleState { alpha: alpha as f32, noise: mode == Reveal::Noise, disturb: title.disturb, colour: title.colour }
    }
}

// ---------------------------------------------------------------------------
// Values

/// `"6s"`, `"800ms"`, `"3:10"`, `"1:02:03.5"`, `"-10s"` or a bare number
/// of seconds.
pub fn parse_time(s: &str) -> Result<f64> {
    let s = s.trim();
    let (sign, body) = match s.strip_prefix('-') {
        Some(rest) => (-1.0, rest.trim()),
        None => (1.0, s),
    };
    let num = |x: &str| x.trim().parse::<f64>().map_err(|_| anyhow!("can't read {s:?} as a time"));
    let secs = if let Some(ms) = body.strip_suffix("ms") {
        num(ms)? / 1000.0
    } else if let Some(secs) = body.strip_suffix('s') {
        num(secs)?
    } else if body.contains(':') {
        let parts: Vec<f64> = body.split(':').map(num).collect::<Result<_>>()?;
        ensure!(parts.len() <= 3, "can't read {s:?} as a time");
        parts.iter().fold(0.0, |acc, p| acc * 60.0 + p)
    } else {
        num(body)?
    };
    ensure!(secs.is_finite(), "can't read {s:?} as a time");
    Ok(sign * secs)
}

fn time_value(v: &Value) -> Result<f64> {
    match v {
        Value::String(s) => parse_time(s),
        Value::Integer(i) => Ok(*i as f64),
        Value::Float(f) => Ok(*f),
        other => bail!("expected a time like \"6s\" or \"1:30\", got {other}"),
    }
}

/// `"#rrggbb"` → sRGB 0..1.
pub fn parse_colour(s: &str) -> Result<[f32; 3]> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    ensure!(hex.len() == 6, "expected a colour like \"#e6e6e6\", got {s:?}");
    let channel = |i: usize| {
        u8::from_str_radix(&hex[i..i + 2], 16)
            .map(|v| v as f32 / 255.0)
            .map_err(|_| anyhow!("expected a colour like \"#e6e6e6\", got {s:?}"))
    };
    Ok([channel(0)?, channel(2)?, channel(4)?])
}

fn srgb_to_linear(c: [f32; 3]) -> [f32; 3] {
    c.map(|v| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) })
}

/// One TOML table being read; `finish` rejects keys nobody asked for, so
/// typos don't silently do nothing.
struct Section {
    label: String,
    table: Table,
}

impl Section {
    fn new(label: &str, value: Value) -> Result<Self> {
        match value {
            Value::Table(table) => Ok(Self { label: label.into(), table }),
            _ => bail!("{label} must be a table"),
        }
    }

    fn take(&mut self, key: &str) -> Option<Value> {
        self.table.remove(key)
    }

    fn string(&mut self, key: &str) -> Result<Option<String>> {
        match self.take(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(other) => bail!("{} {key}: expected a string, got {other}", self.label),
        }
    }

    fn number(&mut self, key: &str) -> Result<Option<f64>> {
        match self.take(key) {
            None => Ok(None),
            Some(Value::Integer(i)) => Ok(Some(i as f64)),
            Some(Value::Float(f)) => Ok(Some(f)),
            Some(other) => bail!("{} {key}: expected a number, got {other}", self.label),
        }
    }

    fn integer(&mut self, key: &str) -> Result<Option<i64>> {
        match self.take(key) {
            None => Ok(None),
            Some(Value::Integer(i)) => Ok(Some(i)),
            Some(other) => bail!("{} {key}: expected a whole number, got {other}", self.label),
        }
    }

    fn time(&mut self, key: &str) -> Result<Option<f64>> {
        let label = &self.label;
        self.table.remove(key).map(|v| time_value(&v).with_context(|| format!("{label} {key}"))).transpose()
    }

    fn pair(&mut self, key: &str) -> Result<Option<[f32; 2]>> {
        match self.take(key) {
            None => Ok(None),
            Some(Value::Array(a)) if a.len() == 2 => {
                let n = |v: &Value| match v {
                    Value::Integer(i) => Some(*i as f32),
                    Value::Float(f) => Some(*f as f32),
                    _ => None,
                };
                match (n(&a[0]), n(&a[1])) {
                    (Some(x), Some(y)) => Ok(Some([x, y])),
                    _ => bail!("{} {key}: expected [x, y] numbers", self.label),
                }
            }
            Some(other) => bail!("{} {key}: expected [x, y], got {other}", self.label),
        }
    }

    fn colour(&mut self, key: &str) -> Result<Option<[f32; 3]>> {
        let label = self.label.clone();
        self.string(key)?.map(|s| parse_colour(&s).with_context(|| format!("{label} {key}"))).transpose()
    }

    fn transition(&mut self) -> Result<Option<Transition>> {
        Ok(match self.string("transition")?.as_deref() {
            None => None,
            Some("noise_dissolve") => Some(Transition::NoiseDissolve),
            Some("dissolve") => Some(Transition::Dissolve),
            Some("cut") => Some(Transition::Cut),
            Some(other) => {
                bail!("{} transition: expected \"noise_dissolve\", \"dissolve\" or \"cut\", got {other:?}", self.label)
            }
        })
    }

    /// Takes every parameter group (`stillness = { … }`, `hiss = { … }`, …).
    fn params(&mut self) -> Result<Settings> {
        let mut out = Vec::new();
        for group in params::GROUPS {
            let Some(value) = self.table.remove(group) else { continue };
            let Value::Table(mut t) = value else {
                bail!("{} {group}: expected a table like {group} = {{ … }}", self.label)
            };
            let at = |key: &str| format!("{} {group}.{key}", self.label);
            // A stillness mode sets all three values; explicit ones then win.
            if group == "stillness"
                && let Some(mode) = t.remove("mode")
            {
                let name = mode.as_str().with_context(|| format!("{}: expected a string", at("mode")))?;
                let mode = Stillness::parse_mode(name).with_context(|| {
                    format!("{}: expected still_at_rest, always_restless or inverted, got {name:?}", at("mode"))
                })?;
                let s = Stillness::preset(mode);
                out.push((P::Baseline as usize, s.baseline));
                out.push((P::Response as usize, s.response));
                out.push((P::Invert as usize, if s.invert { 1.0 } else { 0.0 }));
            }
            if group == "colour"
                && let Some(tint) = t.remove("tint")
            {
                let hex = tint.as_str().with_context(|| format!("{}: expected \"#rrggbb\"", at("tint")))?;
                let rgb = parse_colour(hex).with_context(|| at("tint"))?;
                for (p, v) in [P::TintR, P::TintG, P::TintB].into_iter().zip(rgb) {
                    out.push((p as usize, v));
                }
            }
            for (key, value) in t {
                let i = params::find(group, &key).filter(|&i| SPECS[i].kind != Kind::Channel).with_context(|| {
                    let known: Vec<_> =
                        params::group(group).map(|i| SPECS[i].key).filter(|k| !k.contains('.')).collect();
                    format!("{}: unknown setting (known: {})", at(&key), known.join(", "))
                })?;
                let v = match (SPECS[i].kind, &value) {
                    (Kind::Bool, Value::Boolean(b)) => f32::from(u8::from(*b)),
                    (Kind::Duration, v) => time_value(v).with_context(|| at(&key))? as f32,
                    (Kind::Number, Value::Integer(n)) => *n as f32,
                    (Kind::Number, Value::Float(f)) => *f as f32,
                    (kind, v) => bail!("{}: expected a {kind:?}, got {v}", at(&key)),
                };
                out.push((i, v));
            }
        }
        Ok(out)
    }

    fn finish(self) -> Result<()> {
        match self.table.keys().next() {
            None => Ok(()),
            Some(key) => bail!("{}: unknown key `{key}`", self.label),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(text: &str, audio: f64) -> Timeline {
        Timeline::compile(&TimelineFile::parse(text, Path::new("")).unwrap(), Some(audio))
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("6s").unwrap(), 6.0);
        assert_eq!(parse_time("800ms").unwrap(), 0.8);
        assert_eq!(parse_time("3:10").unwrap(), 190.0);
        assert_eq!(parse_time("1:02:03.5").unwrap(), 3723.5);
        assert_eq!(parse_time("-10s").unwrap(), -10.0);
        assert_eq!(parse_time("12").unwrap(), 12.0);
        assert!(parse_time("soon").is_err());
    }

    #[test]
    fn typos_are_errors() {
        let err = |t: &str| format!("{:#}", TimelineFile::parse(t, Path::new("")).unwrap_err());
        assert!(err("[[cue]]\nat = 1\nphoto = 'a.jpg'\ncrosfade = '2s'").contains("crosfade"));
        assert!(err("[[cue]]\nat = 1\nhiss = { gian = 2 }").contains("gian"));
        assert!(err("[intro]\nscene = 'snow'").contains("snow"));
        assert!(err("[metadata]").contains("metadata"));
    }

    #[test]
    fn parameters_ramp_from_their_cue() {
        let t = compile(
            "[[cue]]\nat = 10\nhiss = { gain = 2.0 }\nramp = '4s'\nease = 'linear'\n\
             [[cue]]\nat = 20\nhiss = { gain = 0.0 }\nramp = 'next'\nease = 'linear'\n\
             [[cue]]\nat = 30\ncolour = { tint = '#ff0000' }\nease = 'step'",
            60.0,
        );
        let gain = |s| t.param_at(P::HissGain as usize, s);
        assert_eq!(gain(5.0), 1.0, "default before any cue");
        assert_eq!(gain(12.0), 1.5, "half way up the ramp");
        assert_eq!(gain(15.0), 2.0);
        assert_eq!(gain(25.0), 1.0, "drifting down until the next cue");
        assert_eq!(gain(35.0), 0.0);
        let p = t.params_at(30.0);
        assert_eq!((p[P::TintR], p[P::TintG]), (1.0, 0.0), "step is immediate");
    }

    #[test]
    fn stillness_mode_expands_and_explicit_values_win() {
        let t = compile("[[cue]]\nat = 0\nease = 'step'\nstillness = { mode = 'inverted', baseline = 0.2 }", 10.0);
        let p = t.params_at(1.0);
        assert_eq!((p[P::Baseline], p[P::Response], p[P::Invert]), (0.2, 0.8, 1.0));
    }

    #[test]
    fn intro_cues_and_outro_make_a_scene_sequence() {
        let t = compile(
            "[intro]\nscene = 'static'\nhold = '10s'\nresolve = '8s'\n\
             [[cue]]\nat = '0:18'\nphoto = 'a.jpg'\n\
             [[cue]]\nat = 30\nphoto = 'b.jpg'\ncrossfade = '4s'\ntransition = 'dissolve'\n\
             [outro]\nscene = 'freeze'\nbegin = '-10s'\ncrossfade = '5s'\ntail = '3s'\nend_on = 'black'",
            60.0,
        );
        assert_eq!(t.photos.len(), 2);
        assert_eq!(t.duration(), Some(63.0));
        assert_eq!(t.scene_at(5.0), SceneMix::steady(Scene::STATIC));
        let resolving = t.scene_at(14.0);
        assert_eq!((resolving.from.kind, resolving.to.kind), (SceneKind::Static, SceneKind::Photo));
        assert_eq!(resolving.progress, 0.5);
        assert_eq!(t.scene_at(20.0).to.layer, 0);
        let crossfade = t.scene_at(32.0);
        assert_eq!((crossfade.from.layer, crossfade.to.layer, crossfade.progress), (0, 1, 0.5));
        assert_eq!(t.scene_at(40.0), SceneMix::steady(t.scene_at(40.0).to));
        // Freeze: the same photo, its disturbance fading to nothing.
        let freezing = t.scene_at(52.5);
        assert_eq!((freezing.from.layer, freezing.to.layer), (1, 1));
        assert!((freezing.gain() - 0.5).abs() < 1e-6);
        assert_eq!(t.scene_at(58.0).to.gain, 0.0);
        assert_eq!(t.scene_at(62.5), SceneMix::steady(Scene::BLACK), "ends on black");
    }

    #[test]
    fn intro_photo_starts_on_the_first_cue() {
        let t = compile("[intro]\nscene = 'photo'\n[[cue]]\nat = 5\nphoto = 'a.jpg'", 20.0);
        assert_eq!(t.scene_at(0.0).to.kind, SceneKind::Photo);
    }

    #[test]
    fn title_reveals_holds_and_vanishes() {
        let t = compile(
            "[title]\ntext = 'still, charged'\nappear = '2s'\nreveal = 'fade'\nreveal_time = '2s'\nhold = '5s'\n\
             vanish = 'noise'\nvanish_time = '2s'",
            30.0,
        );
        assert_eq!(t.title_at(1.0).alpha, 0.0);
        assert_eq!(t.title_at(3.0).alpha, 0.5);
        assert!(!t.title_at(3.0).noise);
        assert_eq!(t.title_at(6.0).alpha, 1.0);
        let vanishing = t.title_at(10.0);
        assert!(vanishing.noise && (vanishing.alpha - 0.5).abs() < 1e-6);
        assert_eq!(t.title_at(12.0).alpha, 0.0);
    }
}
