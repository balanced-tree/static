//! Click-driven signal tears (PLAN §5.2): turns the clicks around a frame into
//! the events the displacement shader draws.
//!
//! Each click becomes one event, seeded by its index in the piece, so every
//! click tears a different band but the same frame always looks the same.

use crate::analysis::ClickEvent;

/// Most events drawn in one frame; the newest win.
pub const MAX_EVENTS: usize = 4;
/// A click stops being drawn after this many decay times.
const LIFETIME_DECAYS: f32 = 3.0;
/// Clicks at least this strong also knock the picture into a vertical roll.
const ROLL_STRENGTH: f32 = 0.8;
/// How long a roll takes to settle back, in seconds.
const ROLL_SECS: f32 = 0.32;
/// Vertical offset of a full-strength roll, as a fraction of the frame height.
pub const ROLL_MAX: f32 = 0.35;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TearEvent {
    /// Index of the click in the piece; seeds where and how it tears.
    pub seed: u32,
    /// The click's strength, 0..1.
    pub strength: f32,
    /// Strength decayed to this frame, 0..1.
    pub intensity: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tears {
    /// Newest first; only the first `count` are valid.
    pub events: [TearEvent; MAX_EVENTS],
    pub count: usize,
    /// Vertical roll, fraction of the frame height (before the tear amount).
    pub roll: f32,
}

/// The tears at frame `index`. `clicks` must be sorted by time (as analysis
/// writes them); `decay` matches the click envelope's.
pub fn at(clicks: &[ClickEvent], index: u32, fps: f32, decay: f32) -> Tears {
    // Same timing as the click envelope: a click anywhere in the frame's
    // interval shows in that frame, ageing from the frame's middle.
    let (mid, end) = ((index as f32 + 0.5) / fps, (index + 1) as f32 / fps);
    let decay = decay.max(1e-3);
    let horizon = (LIFETIME_DECAYS * decay).max(ROLL_SECS);
    let mut tears = Tears::default();
    let newest = clicks.partition_point(|c| c.time < end);
    for (i, c) in clicks[..newest].iter().enumerate().rev() {
        let age = (mid - c.time).max(0.0);
        if age >= horizon {
            break;
        }
        if age < LIFETIME_DECAYS * decay && tears.count < MAX_EVENTS {
            let intensity = c.strength * (-age / decay).exp();
            tears.events[tears.count] = TearEvent { seed: i as u32, strength: c.strength, intensity };
            tears.count += 1;
        }
        if c.strength >= ROLL_STRENGTH && age < ROLL_SECS {
            let settle = 1.0 - age / ROLL_SECS;
            tears.roll = tears.roll.max(ROLL_MAX * c.strength * settle * settle);
        }
    }
    tears
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: f32 = 25.0;

    fn click(time: f32, strength: f32) -> ClickEvent {
        ClickEvent { time, strength }
    }

    #[test]
    fn nothing_without_clicks() {
        assert_eq!(at(&[], 10, FPS, 0.12), Tears::default());
        assert_eq!(at(&[click(5.0, 1.0)], 10, FPS, 0.12), Tears::default(), "click still in the future");
    }

    #[test]
    fn a_click_tears_from_its_own_frame_and_fades() {
        let clicks = [click(0.5, 0.3), click(1.01, 0.6)];
        let t = at(&clicks, 25, FPS, 0.12);
        assert_eq!(t.count, 1, "the 0.5 s click has faded: {t:?}");
        assert_eq!(t.events[0].seed, 1);
        assert!(t.events[0].intensity > 0.55, "full strength in its own frame: {t:?}");
        let later = at(&clicks, 30, FPS, 0.12);
        assert!(later.events[0].intensity < t.events[0].intensity * 0.5);
        assert_eq!(t.roll, 0.0, "too weak to roll");
    }

    #[test]
    fn newest_events_win_and_strong_clicks_roll() {
        let clicks: Vec<_> = (0..6).map(|i| click(1.0 + i as f32 * 0.01, 0.9)).collect();
        let t = at(&clicks, 26, FPS, 0.12);
        assert_eq!(t.count, MAX_EVENTS);
        assert_eq!(t.events[0].seed, 5);
        assert!(t.roll > 0.2, "roll {}", t.roll);
        assert_eq!(at(&clicks, 40, FPS, 0.12).roll, 0.0, "roll has settled");
    }
}
