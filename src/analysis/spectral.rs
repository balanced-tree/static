//! Raw (un-normalised) measurements taken from the signal.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use super::Audio;

const EPS: f32 = 1e-12;
/// Window for the per-video-frame measures: ~85 ms at 48 kHz, fine enough
/// (11.7 Hz bins) to separate mains-hum harmonics.
const FRAME_WINDOW: usize = 4096;
/// Window and hop for onset detection: ~21 ms windows every ~5 ms.
const ONSET_WINDOW: usize = 1024;
const ONSET_HOP: usize = 256;
const MAINS_HZ: f64 = 50.0;
const HUM_HARMONICS: usize = 8;

/// Per-video-frame measurements, one value per frame.
pub struct Measures {
    /// RMS level over the frame's own samples, dBFS.
    pub rms_db: Vec<f32>,
    /// Spectral flatness 100 Hz–16 kHz: ~0.56 for white noise, ~0 for a tone.
    pub flatness: Vec<f32>,
    /// Energy 2–16 kHz, dB (arbitrary reference).
    pub hiband_db: Vec<f32>,
    /// Energy at 50 Hz and its harmonics, dB (arbitrary reference).
    pub hum_db: Vec<f32>,
}

/// Hann-windowed power spectrum of a window centred anywhere in the signal
/// (samples beyond either end count as silence).
struct Spectrum {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    power: Vec<f32>,
}

impl Spectrum {
    fn new(len: usize) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(len);
        let window = (0..len).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / len as f32).cos()).collect();
        let (input, output) = (fft.make_input_vec(), fft.make_output_vec());
        let power = vec![0.0; output.len()];
        Self { fft, window, input, output, power }
    }

    fn compute(&mut self, samples: &[f32], centre: i64) -> &[f32] {
        let start = centre - self.window.len() as i64 / 2;
        for (i, (x, w)) in self.input.iter_mut().zip(&self.window).enumerate() {
            let idx = start + i as i64;
            *x = if (0..samples.len() as i64).contains(&idx) { samples[idx as usize] * w } else { 0.0 };
        }
        self.fft.process(&mut self.input, &mut self.output).expect("FFT buffers sized by the planner");
        for (p, c) in self.power.iter_mut().zip(&self.output) {
            *p = c.norm_sqr();
        }
        &self.power
    }
}

pub fn frame_count(audio: &Audio, fps: f32) -> usize {
    (audio.samples.len() as f64 * fps as f64 / audio.sample_rate as f64).ceil() as usize
}

pub fn measure_frames(audio: &Audio, fps: f32) -> Measures {
    let sr = audio.sample_rate as f64;
    let hop = sr / fps as f64;
    let frames = frame_count(audio, fps);
    let bin_hz = sr / FRAME_WINDOW as f64;
    let max_hz = sr * 0.475;
    let bin = |hz: f64| ((hz.min(max_hz) / bin_hz).round() as usize).max(1);
    let (flat_lo, flat_hi) = (bin(100.0), bin(16_000.0));
    let (hi_lo, hi_hi) = (bin(2_000.0), bin(16_000.0));

    let mut spectrum = Spectrum::new(FRAME_WINDOW);
    let mut m = Measures {
        rms_db: Vec::with_capacity(frames),
        flatness: Vec::with_capacity(frames),
        hiband_db: Vec::with_capacity(frames),
        hum_db: Vec::with_capacity(frames),
    };
    for i in 0..frames {
        let a = (i as f64 * hop) as usize;
        let b = (((i + 1) as f64 * hop) as usize).min(audio.samples.len());
        let block = &audio.samples[a.min(b)..b];
        let rms = (block.iter().map(|x| x * x).sum::<f32>() / block.len().max(1) as f32).sqrt();
        m.rms_db.push(20.0 * (rms + 1e-10).log10());

        let p = spectrum.compute(&audio.samples, ((i as f64 + 0.5) * hop) as i64);
        let band = &p[flat_lo..flat_hi];
        let log_mean = band.iter().map(|x| (x + EPS).ln()).sum::<f32>() / band.len() as f32;
        let mean = band.iter().sum::<f32>() / band.len() as f32 + EPS;
        m.flatness.push(log_mean.exp() / mean);
        m.hiband_db.push(10.0 * (p[hi_lo..hi_hi].iter().sum::<f32>() + EPS).log10());

        let hum: f32 = (1..=HUM_HARMONICS)
            .map(|k| MAINS_HZ * k as f64)
            .filter(|&hz| hz < max_hz)
            .map(|hz| {
                let c = bin(hz);
                p[c - 1..=c + 1].iter().copied().fold(0.0, f32::max)
            })
            .sum();
        m.hum_db.push(10.0 * (hum + EPS).log10());
    }
    m
}

/// Spectral flux on log-compressed magnitudes: how much new energy appears
/// from one short window to the next. Returns the flux series and the time
/// between values, in seconds; value `j` is centred at `j * step`.
pub fn onset_flux(audio: &Audio) -> (Vec<f32>, f32) {
    let mut spectrum = Spectrum::new(ONSET_WINDOW);
    // Hann window sums to N/2, so this scales magnitudes to signal amplitude.
    let norm = 2.0 / ONSET_WINDOW as f32;
    let steps = audio.samples.len() / ONSET_HOP + 1;
    let mut previous: Vec<f32> = Vec::new();
    let mut flux = Vec::with_capacity(steps);
    for j in 0..steps {
        let p = spectrum.compute(&audio.samples, (j * ONSET_HOP) as i64);
        let current: Vec<f32> = p.iter().map(|x| (1.0 + 100.0 * x.sqrt() * norm).ln()).collect();
        let f = if previous.is_empty() {
            0.0
        } else {
            current.iter().zip(&previous).skip(1).map(|(c, p)| (c - p).max(0.0)).sum()
        };
        flux.push(f);
        previous = current;
    }
    (flux, ONSET_HOP as f32 / audio.sample_rate as f32)
}
