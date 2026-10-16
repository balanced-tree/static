//! Play / pause / seek / loop over either the audio clock or, without audio,
//! a wall clock.

use std::time::Instant;

use super::audio::Player;
use crate::analysis::Interleaved;

enum Clock {
    Audio(Player),
    /// `started` is set while running; position = base + time since started.
    Wall {
        base: f64,
        started: Option<Instant>,
    },
}

pub struct Transport {
    clock: Clock,
    duration: Option<f64>,
    /// What the user asked for. The audio clock can stop by itself when the
    /// file runs out; `update` then loops or settles into paused.
    playing: bool,
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
        let mut transport = Self { clock, duration, playing: false, looping: true, loop_in: None, loop_out: None };
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
        self.playing
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        self.run_clock(playing);
    }

    pub fn toggle(&mut self) {
        let (start, end) = self.loop_range();
        if !self.playing && end.is_some_and(|end| self.position() >= end - 1e-3) {
            self.seek(start);
        }
        self.set_playing(!self.playing);
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
        if !self.playing {
            return;
        }
        let ran_out = match &self.clock {
            Clock::Audio(p) => !p.playing(),
            Clock::Wall { .. } => false,
        };
        if self.position() < end && !ran_out {
            return;
        }
        if self.looping {
            self.seek(start);
            self.run_clock(true);
        } else {
            self.set_playing(false);
            self.seek(end);
        }
    }

    fn run_clock(&mut self, run: bool) {
        let position = self.position();
        match &mut self.clock {
            Clock::Audio(p) => p.set_playing(run),
            Clock::Wall { base, started } => {
                *base = position;
                *started = run.then(Instant::now);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_seeks_pauses_and_stops_at_the_end() {
        let mut t = Transport::new(None, Some(10.0));
        t.looping = false;
        t.set_playing(false);
        t.seek(4.0);
        assert_eq!(t.position(), 4.0);
        t.seek(99.0);
        assert_eq!(t.position(), 10.0, "seek clamps to the duration");
        t.set_playing(true);
        t.update();
        assert!(!t.playing(), "stops at the end when not looping");
        t.toggle();
        assert!(t.playing() && t.position() < 1.0, "play from the end restarts");
    }

    #[test]
    fn loop_range_wraps() {
        let mut t = Transport::new(None, Some(10.0));
        t.loop_in = Some(2.0);
        t.loop_out = Some(5.0);
        t.seek(6.0);
        t.update();
        assert!((2.0..2.5).contains(&t.position()), "jumped back to the loop start: {}", t.position());
    }
}
