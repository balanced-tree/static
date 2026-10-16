//! Click / transient detection: adaptive-threshold peak picking on spectral
//! flux.

use super::ClickEvent;

/// Half-width of the moving median the threshold follows.
const MEDIAN_SECS: f32 = 0.15;
/// A peak must be the largest value within this many seconds either side.
const LOCAL_MAX_SECS: f32 = 0.03;
/// Minimum gap between two clicks.
const MIN_GAP_SECS: f32 = 0.05;

pub fn pick(flux: &[f32], step: f32, threshold: f32) -> Vec<ClickEvent> {
    if flux.is_empty() {
        return Vec::new();
    }
    let median_w = (MEDIAN_SECS / step).ceil() as usize;
    let max_w = (LOCAL_MAX_SECS / step).ceil() as usize;
    let min_gap = (MIN_GAP_SECS / step).ceil() as usize;
    // An absolute floor so near-silence can't produce clicks from tiny
    // relative changes.
    let floor = 0.05 * percentile(flux, 0.99) + 1e-3;

    let mut raw = Vec::new();
    let mut last: Option<usize> = None;
    let mut window = Vec::with_capacity(2 * median_w + 1);
    for j in 0..flux.len() {
        let (a, b) = (j.saturating_sub(median_w), (j + median_w + 1).min(flux.len()));
        window.clear();
        window.extend_from_slice(&flux[a..b]);
        window.sort_unstable_by(f32::total_cmp);
        let limit = window[window.len() / 2] * threshold + floor;

        let (ma, mb) = (j.saturating_sub(max_w), (j + max_w + 1).min(flux.len()));
        let is_max = flux[ma..mb].iter().all(|&v| v <= flux[j]);
        let spaced = last.is_none_or(|l| j - l >= min_gap);
        if flux[j] > limit && is_max && spaced {
            raw.push((j, flux[j] - limit));
            last = Some(j);
        }
    }

    // Strength relative to the piece's own typical strong click.
    let mut strengths: Vec<f32> = raw.iter().map(|&(_, s)| s).collect();
    strengths.sort_unstable_by(f32::total_cmp);
    let reference = if strengths.is_empty() { 1.0 } else { percentile(&strengths, 0.9).max(1e-6) };
    raw.into_iter()
        .map(|(j, s)| ClickEvent { time: j as f32 * step, strength: (s / reference).min(1.0) })
        .collect()
}

/// Percentile of `xs` (any order); `q` in 0..1.
pub fn percentile(xs: &[f32], q: f32) -> f32 {
    let mut sorted = xs.to_vec();
    sorted.sort_unstable_by(f32::total_cmp);
    percentile_sorted(&sorted, q)
}

pub fn percentile_sorted(sorted: &[f32], q: f32) -> f32 {
    let idx = ((sorted.len() - 1) as f32 * q).round() as usize;
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_isolated_spikes_and_respects_the_gap() {
        let step = 0.005;
        let mut flux = vec![1.0; 400];
        flux[100] = 20.0;
        flux[104] = 15.0; // within the minimum gap and local-max window
        flux[300] = 10.0;
        let clicks = pick(&flux, step, 1.5);
        assert_eq!(clicks.len(), 2);
        assert!((clicks[0].time - 0.5).abs() < 1e-6 && (clicks[1].time - 1.5).abs() < 1e-6);
        assert!(clicks[0].strength > clicks[1].strength);
    }

    #[test]
    fn flat_flux_has_no_clicks() {
        assert!(pick(&[3.0; 500], 0.005, 1.5).is_empty());
    }
}
