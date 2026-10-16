//! Debug plot: every feature over the whole piece, as an SVG to open in a
//! browser. The faint band is the raw value (min–max per pixel column), the
//! line is what the shaders will see after smoothing.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};

use super::{Features, SmoothingParams};

const WIDTH: f32 = 1800.0;
const LEFT: f32 = 120.0;
const RIGHT: f32 = 20.0;
const LANE: f32 = 90.0;
const GAP: f32 = 18.0;
const TOP: f32 = 40.0;

pub fn write_svg(features: &Features, smoothing: &SmoothingParams, title: &str, path: &Path) -> Result<()> {
    let frames = features.frames(smoothing);
    let smoothed = |f: fn(&super::FeatureFrame) -> f32| frames.iter().map(f).collect::<Vec<_>>();
    let lanes: [(&str, &str, &[f32], Vec<f32>); 6] = [
        ("loud", "#e8e8e8", &features.loud, smoothed(|f| f.loud)),
        ("hiss", "#7fd4ff", &features.hiss, smoothed(|f| f.hiss)),
        ("· flatness", "#4f8aa8", &features.flatness, Vec::new()),
        ("· high band", "#4f8aa8", &features.hiband, Vec::new()),
        ("clicks", "#ff8f6b", &[], smoothed(|f| f.click)),
        ("hum (opt.)", "#a8a070", &features.hum, smoothed(|f| f.hum)),
    ];

    let plot_w = WIDTH - LEFT - RIGHT;
    let height = TOP + lanes.len() as f32 * (LANE + GAP) + 30.0;
    let n = features.frame_count().max(1);
    let x_of = |frame: f32| LEFT + frame / n as f32 * plot_w;

    let mut s = String::new();
    writeln!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" viewBox="0 0 {WIDTH} {height}" font-family="ui-monospace, Menlo, monospace" font-size="12">
<rect width="100%" height="100%" fill="#101010"/>
<text x="{LEFT}" y="24" fill="#e8e8e8" font-size="14">{} · {:.1} s · {} frames @ {} fps · {} clicks</text>"##,
        escape(title),
        features.duration,
        features.frame_count(),
        features.fps,
        features.clicks.len()
    )?;

    // Time grid.
    let step = if features.duration > 300.0 { 60.0 } else if features.duration > 60.0 { 30.0 } else { 5.0 };
    let bottom = TOP + lanes.len() as f32 * (LANE + GAP) - GAP;
    let mut t = 0.0;
    while t <= features.duration {
        let x = x_of(t * features.fps);
        writeln!(
            s,
            r##"<line x1="{x:.1}" y1="{TOP}" x2="{x:.1}" y2="{bottom}" stroke="#2a2a2a"/><text x="{x:.1}" y="{:.1}" fill="#777" text-anchor="middle">{}:{:02}</text>"##,
            bottom + 18.0,
            (t / 60.0) as u32,
            (t % 60.0) as u32
        )?;
        t += step;
    }

    for (lane, (label, colour, raw, line)) in lanes.iter().enumerate() {
        let y0 = TOP + lane as f32 * (LANE + GAP);
        let y_of = |v: f32| y0 + LANE - v.clamp(0.0, 1.0) * LANE;
        writeln!(
            s,
            r##"<rect x="{LEFT}" y="{y0}" width="{plot_w}" height="{LANE}" fill="none" stroke="#2a2a2a"/><text x="10" y="{:.1}" fill="{colour}">{label}</text>"##,
            y0 + LANE / 2.0 + 4.0
        )?;

        let cols = plot_w as usize;
        let range = |c: usize| (c * n / cols, ((c + 1) * n / cols).max(c * n / cols + 1).min(n));
        if !raw.is_empty() {
            let (mut top, mut bot) = (String::new(), String::new());
            for c in 0..cols {
                let (a, b) = range(c);
                let slice = &raw[a.min(raw.len())..b.min(raw.len())];
                let (lo, hi) = slice.iter().fold((1.0f32, 0.0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)));
                let x = LEFT + c as f32 + 0.5;
                write!(top, "{x:.1},{:.1} ", y_of(hi))?;
                bot.insert_str(0, &format!("{x:.1},{:.1} ", y_of(lo)));
            }
            writeln!(s, r#"<polygon points="{top}{bot}" fill="{colour}" fill-opacity="0.25"/>"#)?;
        }
        if !line.is_empty() {
            let mut pts = String::new();
            for c in 0..cols {
                let (a, b) = range(c);
                let slice = &line[a.min(line.len())..b.min(line.len())];
                let v = slice.iter().copied().fold(0.0, f32::max);
                write!(pts, "{:.1},{:.1} ", LEFT + c as f32 + 0.5, y_of(v))?;
            }
            writeln!(s, r#"<polyline points="{pts}" fill="none" stroke="{colour}" stroke-width="1.2"/>"#)?;
        }
        if *label == "clicks" {
            for c in &features.clicks {
                let x = x_of(c.time * features.fps);
                writeln!(
                    s,
                    r##"<line x1="{x:.1}" y1="{:.1}" x2="{x:.1}" y2="{:.1}" stroke="#ffd0c0" stroke-width="0.8"/>"##,
                    y_of(0.0),
                    y_of(c.strength)
                )?;
            }
        }
    }
    s.push_str("</svg>\n");
    std::fs::write(path, s).with_context(|| format!("writing {}", path.display()))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
