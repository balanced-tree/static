//! Play / pause / seek / loop over either the audio clock or, without audio,
//! a wall clock. Past the end of the audio (the outro tail) the audio clock
//! hands over to a wall clock until the video ends.

use std::time::Instant;

use super::audio::Player;
use crate::analysis::Interleaved;

/// Position = base + time since `started` (set while running).
#[derive(Clone, Copy, Debug)]
struct Wall {
    base: f64,
    started: Option<Instant>,
}

impl Wall {
    fn new(base: f64, running: bool) -> Self {
        Self { base, started: running.then(Instant::now) }
    }

    fn position(&self) -> f64 {
        self.base + self.started.map_or(0.0, |s| s.elapsed().as_secs_f64())
    }

    fn run(&mut self, run: bool) {
        *self = Self::new(self.position(), run);
    }

    fn seek(&mut self, secs: f64) {
        *self = Self::new(secs, self.started.is_some());
    }
}

enum Clock {
    Audio {
        player: Player,
        /// Length of the audio in seconds.
        length: f64,
        /// Set while the playhead is past the end of the audio.
        past_end: Option<Wall>,
    },
    Wall(Wall),
}

pub struct Transport {
    clock: Clock,
    duration: Option<f64>,
    /// What the user asked for. The audio clock can stop by itself when the
    /// file runs out; `update` then carries on, loops or settles into paused.
    playing: bool,
    pub looping: bool,
    pub loop_in: Option<f64>,
    pub loop_out: Option<f64>,
}

impl Transport {
    /// `duration` is the whole video (it may run past the audio). Falls back
    /// to a silent wall clock if there is no audio or no device.
    pub fn new(audio: Option<Interleaved>, duration: Option<f64>) -> Self {
        let clock = match audio.map(|a| (a.duration(), Player::new(a))) {
            Some((length, Ok(player))) => Clock::Audio { player, length, past_end: None },
            Some((_, Err(e))) => {
                eprintln!("warning: no audio playback ({e:#}); previewing silently");
                Clock::Wall(Wall::new(0.0, false))
            }
            None => Clock::Wall(Wall::new(0.0, false)),
        };
        let mut transport = Self { clock, duration, playing: false, looping: true, loop_in: None, loop_out: None };
        transport.set_playing(true);
        transport
    }

    pub fn has_audio(&self) -> bool {
        matches!(self.clock, Clock::Audio { .. })
    }

    pub fn duration(&self) -> Option<f64> {
        self.duration
    }

    pub fn set_duration(&mut self, duration: Option<f64>) {
        self.duration = duration;
        if let Some(d) = duration
            && self.position() > d
        {
            self.seek(d);
        }
    }

    pub fn position(&self) -> f64 {
        match &self.clock {
            Clock::Audio { past_end: Some(wall), .. } | Clock::Wall(wall) => wall.position(),
            Clock::Audio { player, .. } => player.position(),
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
        let playing = self.playing;
        match &mut self.clock {
            Clock::Audio { player, length, past_end } => {
                if secs >= *length {
                    player.seek(*length);
                    player.set_playing(false);
                    *past_end = Some(Wall::new(secs, playing));
                } else {
                    *past_end = None;
                    player.seek(secs);
                    player.set_playing(playing);
                }
            }
            Clock::Wall(wall) => wall.seek(secs),
        }
    }

    /// Start and end of the playable range: the loop points if set, else the
    /// whole piece (`None` end = open-ended, without audio or features).
    pub fn loop_range(&self) -> (f64, Option<f64>) {
        let start = self.loop_in.unwrap_or(0.0);
        let end = self.loop_out.or(self.duration).filter(|&e| e > start);
        (start, end)
    }

    /// Call once per frame: continues past the audio, wraps around the loop
    /// or stops at the end.
    pub fn update(&mut self) {
        if !self.playing {
            return;
        }
        // The audio ran out but the video goes on: hand over to the wall clock.
        if let Clock::Audio { player, length, past_end: past_end @ None } = &mut self.clock
            && !player.playing()
        {
            *past_end = Some(Wall::new(*length, true));
        }
        let (start, Some(end)) = self.loop_range() else { return };
        if self.position() < end {
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
        match &mut self.clock {
            Clock::Audio { past_end: Some(wall), .. } | Clock::Wall(wall) => wall.run(run),
            Clock::Audio { player, .. } => player.set_playing(run),
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
