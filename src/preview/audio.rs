//! Audio playback for the preview. The playback position is the preview's
//! master clock, so picture and sound stay locked together.
//!
//! The device callback owns the read position; the UI talks to it through
//! atomics only (no locks on the audio thread).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample as _, SampleFormat, SizedSample};

use crate::analysis::Interleaved;

/// Marks "no seek pending" in `Shared::seek`.
const NO_SEEK: u64 = u64::MAX;

struct Shared {
    /// Read position in source frames (f64 bits). Written by the callback.
    position: AtomicU64,
    /// Requested position in source frames (f64 bits), or `NO_SEEK`.
    seek: AtomicU64,
    playing: AtomicBool,
}

pub struct Player {
    _stream: cpal::Stream,
    shared: Arc<Shared>,
    sample_rate: f64,
}

impl Player {
    pub fn new(audio: Interleaved) -> Result<Self> {
        let device = cpal::default_host().default_output_device().context("no audio output device")?;
        let supported = device.default_output_config().context("querying the audio output")?;
        let config = supported.config();
        let shared = Arc::new(Shared {
            position: AtomicU64::new(0f64.to_bits()),
            seek: AtomicU64::new(NO_SEEK),
            playing: AtomicBool::new(false),
        });
        let sample_rate = audio.sample_rate as f64;
        let source = Source {
            ratio: sample_rate / config.sample_rate as f64,
            channels: audio.channels as usize,
            samples: audio.samples,
        };
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &config, source, shared.clone())?,
            SampleFormat::I16 => build::<i16>(&device, &config, source, shared.clone())?,
            SampleFormat::U16 => build::<u16>(&device, &config, source, shared.clone())?,
            SampleFormat::I32 => build::<i32>(&device, &config, source, shared.clone())?,
            other => bail!("unsupported audio output format {other}"),
        };
        stream.play().context("starting audio output")?;
        Ok(Self { _stream: stream, shared, sample_rate })
    }

    /// Seconds; reflects a pending seek immediately.
    pub fn position(&self) -> f64 {
        let pending = self.shared.seek.load(Ordering::Acquire);
        let frames = if pending != NO_SEEK { pending } else { self.shared.position.load(Ordering::Acquire) };
        f64::from_bits(frames) / self.sample_rate
    }

    pub fn seek(&self, secs: f64) {
        self.shared.seek.store((secs.max(0.0) * self.sample_rate).to_bits(), Ordering::Release);
    }

    pub fn playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }

    pub fn set_playing(&self, playing: bool) {
        self.shared.playing.store(playing, Ordering::Release);
    }
}

struct Source {
    samples: Vec<f32>,
    channels: usize,
    /// Source frames per output frame.
    ratio: f64,
}

impl Source {
    /// Linear interpolation is plenty for monitoring.
    fn sample(&self, pos: f64, channel: usize) -> f32 {
        let i = pos as usize;
        let t = (pos - i as f64) as f32;
        let a = self.samples[i * self.channels + channel];
        let b = self.samples[(i + 1) * self.channels + channel];
        a + (b - a) * t
    }
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    source: Source,
    shared: Arc<Shared>,
) -> Result<cpal::Stream> {
    let out_channels = config.channels as usize;
    let last = (source.samples.len() / source.channels).saturating_sub(1) as f64;
    let stream = device.build_output_stream::<T, _, _>(
        config.clone(),
        move |out: &mut [T], _| {
            let seek = shared.seek.swap(NO_SEEK, Ordering::AcqRel);
            let mut pos = f64::from_bits(if seek != NO_SEEK { seek } else { shared.position.load(Ordering::Acquire) });
            let playing = shared.playing.load(Ordering::Acquire);
            for frame in out.chunks_mut(out_channels) {
                if !playing || pos >= last {
                    frame.fill(T::EQUILIBRIUM);
                    continue;
                }
                for (c, o) in frame.iter_mut().enumerate() {
                    // Mono feeds every output; otherwise channel to channel.
                    *o = match source.channels {
                        1 => T::from_sample(source.sample(pos, 0)),
                        n if c < n => T::from_sample(source.sample(pos, c)),
                        _ => T::EQUILIBRIUM,
                    };
                }
                pos += source.ratio;
            }
            // Stop at the end, unless the UI has already asked to jump.
            if playing && pos >= last && shared.seek.load(Ordering::Acquire) == NO_SEEK {
                shared.playing.store(false, Ordering::Release);
            }
            shared.position.store(pos.min(last).to_bits(), Ordering::Release);
        },
        |e| eprintln!("audio error: {e}"),
        None,
    )?;
    Ok(stream)
}
