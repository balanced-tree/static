//! The preview's egui layer: transport bar with scrub strip and cue markers,
//! parameter panel, feature meters and keyboard shortcuts.
//!
//! Sliders show each parameter's value at the playhead. Moving one sets an
//! *override*: that value then holds for the whole piece (instead of the
//! timeline's) until cleared. "Copy as TOML" turns the overrides into a cue
//! to paste into `timeline.toml`.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use egui::{Color32, Key, Pos2, Rect, Sense, Slider, Stroke, StrokeKind, Vec2};

use super::Session;
use crate::FPS;
use crate::look::{Stillness, StillnessMode};
use crate::params::{self, Kind, P, SPECS};

/// How long a non-error status message stays up.
const STATUS_TIME: Duration = Duration::from_secs(4);

/// UI-only state.
pub struct UiState {
    pub show_panels: bool,
    pub show_meters: bool,
    /// Where the frame may be drawn (points), set each frame.
    pub view_rect: Rect,
    /// Width / height of the rendered frame.
    pub frame_aspect: f32,
    pub quit: bool,
    copied_at: Option<f64>,
    /// Latest hot-reload message: (text, is error, when).
    status: Option<(String, bool, Instant)>,
}

impl UiState {
    pub fn new(frame_aspect: f32) -> Self {
        Self {
            show_panels: true,
            show_meters: true,
            view_rect: Rect::NOTHING,
            frame_aspect,
            quit: false,
            copied_at: None,
            status: None,
        }
    }

    pub fn set_status(&mut self, text: String, error: bool) {
        self.status = (!text.is_empty()).then(|| (text, error, Instant::now()));
    }
}

const ACCENT: Color32 = Color32::from_rgb(255, 143, 107);
const HISS: Color32 = Color32::from_rgb(127, 212, 255);
const MARKER: Color32 = Color32::from_rgb(200, 190, 120);
const ERROR: Color32 = Color32::from_rgb(255, 110, 110);

pub fn draw(ui: &mut egui::Ui, s: &mut Session) {
    keys(ui, s);
    if s.ui.show_panels {
        egui::Panel::bottom("transport").show(ui, |ui| transport_bar(ui, s));
        egui::Panel::right("controls").resizable(false).exact_size(300.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| controls(ui, s));
        });
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        s.ui.view_rect = ui.max_rect();
        if s.ui.show_meters {
            meters(ui, s);
        }
        status(ui, s);
    });
}

fn keys(ui: &mut egui::Ui, s: &mut Session) {
    if ui.ctx().egui_wants_keyboard_input() {
        return;
    }
    let pressed = |k| ui.input(|i| i.key_pressed(k));
    let shift = ui.input(|i| i.modifiers.shift);
    let t = &mut s.transport;
    let step = if shift { 1.0 / FPS as f64 } else { 5.0 };
    if pressed(Key::Space) {
        t.toggle();
    }
    if pressed(Key::ArrowLeft) {
        t.seek(t.position() - step);
    }
    if pressed(Key::ArrowRight) {
        t.seek(t.position() + step);
    }
    if pressed(Key::Home) {
        t.seek(t.loop_range().0);
    }
    // Jump between cues. A little slack, so "previous" from just after a
    // marker goes to the one before it.
    let markers = &s.show.timeline().markers;
    if pressed(Key::OpenBracket) {
        let pos = t.position() - 0.25;
        t.seek(markers.iter().rev().find(|m| m.time < pos).map_or(0.0, |m| m.time));
    }
    if pressed(Key::CloseBracket) {
        let pos = t.position() + 1e-3;
        if let Some(m) = markers.iter().find(|m| m.time > pos) {
            t.seek(m.time);
        }
    }
    if pressed(Key::L) {
        t.looping = !t.looping;
    }
    if pressed(Key::I) {
        t.loop_in = Some(t.position());
    }
    if pressed(Key::O) {
        t.loop_out = Some(t.position());
    }
    if pressed(Key::H) {
        s.ui.show_panels = !s.ui.show_panels;
    }
    if pressed(Key::M) {
        s.ui.show_meters = !s.ui.show_meters;
    }
    if pressed(Key::Escape) {
        s.ui.quit = true;
    }
}

fn transport_bar(ui: &mut egui::Ui, s: &mut Session) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let t = &mut s.transport;
        if ui.button(if t.playing() { "Pause" } else { "Play" }).clicked() {
            t.toggle();
        }
        let pos = t.position();
        let total = t.duration().map_or("—".into(), clock);
        ui.monospace(format!("{} / {total}   frame {}", clock(pos), (pos * FPS as f64) as u32));
        ui.separator();
        ui.checkbox(&mut t.looping, "Loop");
        if ui.button("In").on_hover_text("Set loop start here [I]").clicked() {
            t.loop_in = Some(pos);
        }
        if ui.button("Out").on_hover_text("Set loop end here [O]").clicked() {
            t.loop_out = Some(pos);
        }
        if (t.loop_in.is_some() || t.loop_out.is_some()) && ui.button("Clear").clicked() {
            (t.loop_in, t.loop_out) = (None, None);
        }
        ui.separator();
        let current = s.show.timeline().markers.iter().rev().find(|m| m.time <= pos + 1e-3);
        ui.label(current.map_or("start", |m| m.label.as_str()));
        ui.separator();
        ui.weak(if t.has_audio() { "audio" } else { "silent clock (no audio)" });
    });
    scrub_strip(ui, s);
    ui.add_space(4.0);
}

/// Loudness envelope, click ticks, cue markers, loop region and playhead.
/// Click or drag to seek.
fn scrub_strip(ui: &mut egui::Ui, s: &mut Session) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 50.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_gray(18));
    let Some(duration) = s.transport.duration().filter(|d| *d > 0.0) else {
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, "no audio", Default::default(), Color32::GRAY);
        return;
    };
    let x_of = |secs: f64| rect.left() + (secs / duration) as f32 * rect.width();

    let (start, end) = s.transport.loop_range();
    if s.transport.loop_in.is_some() || s.transport.loop_out.is_some() {
        let r = Rect::from_x_y_ranges(x_of(start)..=x_of(end.unwrap_or(duration)), rect.y_range());
        painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(255, 255, 255, 14));
    }

    // The analysed audio, which may stop before the video does (outro tail).
    if let Some(f) = s.show.features() {
        let n = f.loud.len().max(1);
        let audio_right = x_of(f.duration as f64);
        let cols = (audio_right - rect.left()).max(1.0) as usize;
        let (lane_top, lane_bottom) = (rect.top() + 14.0, rect.bottom() - 8.0);
        for c in 0..cols {
            let (a, b) = (c * n / cols, ((c + 1) * n / cols).max(c * n / cols + 1).min(n));
            let loud = f.loud[a.min(n - 1)..b].iter().copied().fold(0.0, f32::max);
            let hiss = f.hiss[a.min(n - 1)..b].iter().copied().fold(0.0, f32::max);
            let x = rect.left() + c as f32 + 0.5;
            let y = |v: f32| lane_bottom - v * (lane_bottom - lane_top);
            painter.line_segment([Pos2::new(x, lane_bottom), Pos2::new(x, y(loud))], (1.0, Color32::from_gray(70)));
            painter.line_segment(
                [Pos2::new(x, lane_bottom), Pos2::new(x, y(hiss * loud))],
                (1.0, HISS.gamma_multiply(0.5)),
            );
        }
        for click in &f.clicks {
            let x = x_of(click.time as f64);
            painter.line_segment(
                [Pos2::new(x, rect.top() + 2.0), Pos2::new(x, rect.top() + 2.0 + 10.0 * click.strength)],
                (1.0, ACCENT),
            );
        }
    }

    for m in &s.show.timeline().markers {
        let x = x_of(m.time);
        let tip = Pos2::new(x, rect.bottom() - 7.0);
        painter.add(egui::Shape::convex_polygon(
            vec![tip, Pos2::new(x - 4.0, rect.bottom()), Pos2::new(x + 4.0, rect.bottom())],
            MARKER,
            Stroke::NONE,
        ));
    }

    let px = x_of(s.transport.position());
    painter.line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(2.0, Color32::WHITE));

    if let Some(p) = response.interact_pointer_pos()
        && (response.clicked() || response.dragged())
    {
        s.transport.seek(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * duration);
    }
    let hover = response.hover_pos().and_then(|p| {
        s.show.timeline().markers.iter().find(|m| (x_of(m.time) - p.x).abs() < 5.0 && p.y > rect.bottom() - 10.0)
    });
    let tip = match hover {
        Some(m) => format!("{} at {}", m.label, clock(m.time)),
        None => "Loudness (grey), hiss (blue), clicks (orange), cues (yellow). Click or drag to seek.".into(),
    };
    response.on_hover_text(tip);
}

fn controls(ui: &mut egui::Ui, s: &mut Session) {
    ui.add_space(6.0);
    let secs = s.transport.position();
    let values = s.show.params_at(secs);

    ui.horizontal(|ui| {
        ui.label("Stillness preset");
        egui::ComboBox::from_id_salt("preset").selected_text("apply…").show_ui(ui, |ui| {
            for mode in [StillnessMode::StillAtRest, StillnessMode::AlwaysRestless, StillnessMode::Inverted] {
                if ui.selectable_label(false, mode_name(mode)).clicked() {
                    let p = Stillness::preset(mode);
                    s.show.set_override(P::Baseline as usize, Some(p.baseline));
                    s.show.set_override(P::Response as usize, Some(p.response));
                    s.show.set_override(P::Invert as usize, Some(if p.invert { 1.0 } else { 0.0 }));
                }
            }
        });
    });

    for group in params::GROUPS {
        let overridden = params::group(group).any(|i| s.show.overrides()[i].is_some());
        let title = if overridden { format!("{} •", heading(group)) } else { heading(group).to_string() };
        egui::CollapsingHeader::new(title).id_salt(group).default_open(true).show(ui, |ui| {
            for i in params::group(group) {
                param_row(ui, s, i, values.0[i]);
            }
        });
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("seed");
        ui.add(egui::DragValue::new(&mut s.settings.seed)).on_hover_text("Noise seed ([meta] seed)");
    });

    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui
            .button("Copy as TOML")
            .on_hover_text("Copies the overrides as a cue at the playhead (also printed to the terminal)")
            .clicked()
        {
            let toml = to_toml(s, secs);
            println!("\n{toml}");
            ui.ctx().copy_text(toml);
            s.ui.copied_at = Some(ui.input(|i| i.time));
        }
        if s.ui.copied_at.is_some_and(|t| ui.input(|i| i.time) - t < 2.0) {
            ui.weak("copied");
        }
        if ui.button("Clear overrides").clicked() {
            s.show.clear_overrides();
        }
    });

    ui.add_space(12.0);
    ui.weak(
        "Space play/pause · ←/→ 5 s (⇧ 1 frame) · [ ] previous/next cue · Home loop start\n\
         L loop · I/O loop in/out · H hide panels · M meters · Esc quit\n\
         Sliders override the timeline (• marks a group with overrides; × clears one).",
    );
}

/// One parameter's control. Colour channels share one picker, on the red row.
fn param_row(ui: &mut egui::Ui, s: &mut Session, i: usize, value: f32) {
    let spec = &SPECS[i];
    let overridden = s.show.overrides()[i].is_some();
    ui.horizontal(|ui| {
        match spec.kind {
            Kind::Channel if i == P::TintR as usize => {
                let channels = [P::TintR as usize, P::TintG as usize, P::TintB as usize];
                let current = s.show.params_at(s.transport.position());
                let mut rgb = channels.map(|c| current.0[c]);
                ui.label("tint");
                if ui.color_edit_button_rgb(&mut rgb).changed() {
                    for (c, v) in channels.into_iter().zip(rgb) {
                        s.show.set_override(c, Some(v));
                    }
                }
                if channels.iter().any(|&c| s.show.overrides()[c].is_some()) && clear_button(ui) {
                    for c in channels {
                        s.show.set_override(c, None);
                    }
                }
                return;
            }
            Kind::Channel => return,
            Kind::Bool => {
                let mut on = value > 0.5;
                if ui.checkbox(&mut on, spec.key).changed() {
                    s.show.set_override(i, Some(if on { 1.0 } else { 0.0 }));
                }
            }
            Kind::Number | Kind::Duration => {
                let mut v = value;
                let label = if spec.kind == Kind::Duration { format!("{} s", spec.key) } else { spec.key.to_string() };
                let mut slider = Slider::new(&mut v, spec.min..=spec.max).text(label);
                if spec.min > 0.0 && spec.max / spec.min >= 8.0 {
                    slider = slider.logarithmic(true);
                }
                if i == P::SnowRows as usize {
                    slider = slider.integer();
                }
                if ui.add(slider).changed() {
                    s.show.set_override(i, Some(v));
                }
            }
        }
        if overridden && clear_button(ui) {
            s.show.set_override(i, None);
        }
    });
}

fn clear_button(ui: &mut egui::Ui) -> bool {
    ui.small_button("×").on_hover_text("Clear the override (back to the timeline)").clicked()
}

/// Feature and disturbance bars over the bottom-left of the frame.
fn meters(ui: &mut egui::Ui, s: &Session) {
    let frame = fit(s.ui.view_rect, s.ui.frame_aspect);
    let index = s.frame_index();
    let f = s.show.features_at(index);
    let inputs = s.show.inputs(index);
    let d = inputs.disturbance;
    let rows = [
        ("hiss", f.hiss, HISS),
        ("click", f.click, ACCENT),
        ("loud", f.loud, Color32::from_gray(220)),
        ("→ snow", d.snow, Color32::WHITE),
        ("→ jitter", d.jitter, Color32::WHITE),
        ("→ tear", d.tear, Color32::WHITE),
        ("→ chroma", d.chroma, Color32::WHITE),
        ("roll", inputs.tears.roll / crate::tears::ROLL_MAX, ACCENT),
        ("title", inputs.title.alpha, MARKER),
    ];
    let (w, row_h) = (180.0, 16.0);
    let panel = Rect::from_min_size(
        Pos2::new(frame.left() + 12.0, frame.bottom() - 12.0 - rows.len() as f32 * row_h - 8.0),
        Vec2::new(w, rows.len() as f32 * row_h + 8.0),
    );
    let painter = ui.painter();
    painter.rect_filled(panel, 4.0, Color32::from_black_alpha(170));
    for (i, (label, value, colour)) in rows.iter().enumerate() {
        let y = panel.top() + 4.0 + i as f32 * row_h;
        painter.text(
            Pos2::new(panel.left() + 8.0, y + row_h / 2.0),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::monospace(11.0),
            Color32::LIGHT_GRAY,
        );
        let bar = Rect::from_min_size(Pos2::new(panel.left() + 72.0, y + 4.0), Vec2::new(w - 82.0, row_h - 8.0));
        painter.rect_stroke(bar, 1.0, (1.0, Color32::from_gray(80)), StrokeKind::Inside);
        let filled = Rect::from_min_size(bar.min, Vec2::new(bar.width() * value.clamp(0.0, 1.0), bar.height()));
        painter.rect_filled(filled, 1.0, *colour);
    }
}

/// Hot-reload messages, top-left of the frame. Errors stay until the next
/// reload; other messages fade out.
fn status(ui: &mut egui::Ui, s: &Session) {
    let Some((text, error, when)) = &s.ui.status else { return };
    if !error && when.elapsed() > STATUS_TIME {
        return;
    }
    let frame = fit(s.ui.view_rect, s.ui.frame_aspect);
    let font = egui::FontId::monospace(12.0);
    let colour = if *error { ERROR } else { Color32::LIGHT_GRAY };
    let galley = ui.painter().layout(text.clone(), font, colour, frame.width() - 40.0);
    let pos = frame.left_top() + Vec2::new(12.0, 12.0);
    let bg = Rect::from_min_size(pos, galley.size()).expand(6.0);
    ui.painter().rect_filled(bg, 4.0, Color32::from_black_alpha(200));
    ui.painter().galley(pos, galley, colour);
}

/// Largest rect of the given aspect centred in `area` (matches the blit).
pub fn fit(area: Rect, aspect: f32) -> Rect {
    let size = if area.width() / area.height() > aspect {
        Vec2::new(area.height() * aspect, area.height())
    } else {
        Vec2::new(area.width(), area.width() / aspect)
    };
    Rect::from_center_size(area.center(), size)
}

fn heading(group: &str) -> &'static str {
    match group {
        "stillness" => "Stillness",
        "snow_mix" => "Snow mix",
        "jitter_mix" => "Jitter mix",
        "tear_mix" => "Tear mix",
        "chroma_mix" => "Chroma split mix",
        "hiss" => "Hiss",
        "loud" => "Loudness",
        "clicks" => "Clicks",
        "snow" => "Snow",
        "crt" => "CRT",
        "colour" => "Colour",
        _ => "Other",
    }
}

fn mode_name(mode: StillnessMode) -> &'static str {
    match mode {
        StillnessMode::StillAtRest => "still at rest",
        StillnessMode::AlwaysRestless => "always restless",
        StillnessMode::Inverted => "inverted",
    }
}

fn clock(secs: f64) -> String {
    format!("{}:{:05.2}", (secs / 60.0) as u32, secs % 60.0)
}

/// A `[[cue]]` at `secs` holding the overridden parameters, or every
/// parameter's current value if nothing is overridden.
fn to_toml(s: &Session, secs: f64) -> String {
    let values = s.show.params_at(secs);
    let overrides = s.show.overrides();
    let any = overrides.iter().any(Option::is_some);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# from `static preview`: {}",
        if any { "the overridden values" } else { "every value at the playhead (no overrides)" }
    );
    let _ = writeln!(out, "[[cue]]\nat = \"{}\"", clock(secs));
    for group in params::GROUPS {
        let mut fields = Vec::new();
        for i in params::group(group) {
            let spec = &SPECS[i];
            if any && overrides[i].is_none() && spec.kind != Kind::Channel {
                continue;
            }
            let v = values.0[i];
            match spec.kind {
                Kind::Number if i == P::SnowRows as usize => fields.push(format!("{} = {}", spec.key, v.round())),
                Kind::Number => fields.push(format!("{} = {v:.2}", spec.key)),
                Kind::Duration => fields.push(format!("{} = \"{}ms\"", spec.key, (v * 1000.0).round())),
                Kind::Bool => fields.push(format!("{} = {}", spec.key, v > 0.5)),
                Kind::Channel if i == P::TintR as usize => {
                    let channels = [P::TintR, P::TintG, P::TintB];
                    if !any || channels.iter().any(|&c| overrides[c as usize].is_some()) {
                        let hex = channels.map(|c| (values[c].clamp(0.0, 1.0) * 255.0).round() as u8);
                        fields.push(format!("tint = \"#{:02x}{:02x}{:02x}\"", hex[0], hex[1], hex[2]));
                    }
                }
                Kind::Channel => {}
            }
        }
        if !fields.is_empty() {
            let _ = writeln!(out, "{group} = {{ {} }}", fields.join(", "));
        }
    }
    let _ = write!(out, "# seed = {} (goes in [meta])", s.settings.seed);
    out
}
