//! Play / pause / seek / loop over either the audio clock or, without audio,
//! a wall clock.

use std::time::Instant;

use super::audio::Player;
use crate::analysis::Interleaved;

enum Clock {
    Audio(Player),
    /// `started` is set while playing; position = base + time since started.
    Wall { base: f64, started: Option<Instant> },
}

pub struct Transport {
    clock: Clock,
    duration: Option<f64>,
    pub looping: bool,
    pub loop_in: Option<f64>,
    pub loop_out: Option<f64>,
}

impl Transport {
    /// Falls back to a silent wall clock if there is no audio or no device.
    pub fn new(audio: Option<Interleaved>, features_duration: Option<f64>) -> Self {
        let duration = audio.as_ref().map(|a| a.duration()).or(features_duration);
        let clock = match audio.map(Player::new) {
            Some(Ok(player)) => Clock::Audio(player),
            Some(Err(e)) => {
                eprintln!("warning: no audio playback ({e:#}); previewing silently");
                Clock::Wall { base: 0.0, started: None }
            }
            None => Clock::Wall { base: 0.0, started: None },
        };
        let mut transport = Self { clock, duration, looping: true, loop_in: None, loop_out: None };
        transport.set_playing(true);
        transport
    }

    pub fn has_audio(&self) -> bool {
        matches!(self.clock, Clock::Audio(_))
    }

    pub fn duration(&self) -> Option<f64> {
        self.duration
    }

    pub fn position(&self) -> f64 {
        match &self.clock {
            Clock::Audio(p) => p.position(),
            Clock::Wall { base, started } => base + started.map_or(0.0, |s| s.elapsed().as_secs_f64()),
        }
    }

    pub fn playing(&self) -> bool {
        match &self.clock {
            Clock::Audio(p) => p.playing(),
            Clock::Wall { started, .. } => started.is_some(),
        }
    }

    pub fn set_playing(&mut self, playing: bool) {
        let position = self.position();
        match &mut self.clock {
            Clock::Audio(p) => p.set_playing(playing),
            Clock::Wall { base, started } => {
                *base = position;
                *started = playing.then(Instant::now);
            }
        }
    }

    pub fn toggle(&mut self) {
        let (start, end) = self.loop_range();
        if !self.playing() && end.is_some_and(|end| self.position() >= end - 1e-3) {
            self.seek(start);
        }
        self.set_playing(!self.playing());
    }

    pub fn seek(&mut self, secs: f64) {
        let secs = self.duration.map_or(secs, |d| secs.min(d)).max(0.0);
        match &mut self.clock {
            Clock::Audio(p) => p.seek(secs),
            Clock::Wall { base, started } => {
                *base = secs;
                if started.is_some() {
                    *started = Some(Instant::now());
                }
            }
        }
    }

    /// Start and end of the playable range: the loop points if set, else the
    /// whole piece (`None` end = open-ended, without audio or features).
    pub fn loop_range(&self) -> (f64, Option<f64>) {
        let start = self.loop_in.unwrap_or(0.0);
        let end = self.loop_out.or(self.duration).filter(|&e| e > start);
        (start, end)
    }

    /// Call once per frame: wraps around the loop or stops at the end.
    pub fn update(&mut self) {
        let (start, Some(end)) = self.loop_range() else { return };
        let position = self.position();
        let ended = position >= end || (self.has_audio() && !self.playing() && self.at_audio_end());
        if !ended {
            return;
        }
        if self.looping && (self.playing() || self.at_audio_end()) {
            self.seek(start);
            self.set_playing(true);
        } else if position >= end && self.playing() {
            self.set_playing(false);
            self.seek(end);
        }
    }

    /// The audio callback stops by itself when the file runs out.
    fn at_audio_end(&self) -> bool {
        self.duration.is_some_and(|d| self.position() >= d - 0.05)
    }
}
