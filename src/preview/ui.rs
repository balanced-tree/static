//! The preview's egui layer: transport bar with scrub strip, parameter panel,
//! feature meters and keyboard shortcuts.

use std::fmt::Write as _;

use egui::{Color32, Key, Pos2, Rect, Sense, Slider, Stroke, StrokeKind, Vec2};

use super::Session;
use crate::FPS;
use crate::analysis::Envelope;
use crate::look::{Stillness, StillnessMode, Weights};
use crate::renderer::Crt;

/// UI-only state.
pub struct UiState {
    pub show_panels: bool,
    pub show_meters: bool,
    pub preset: StillnessMode,
    /// Where the frame may be drawn (points), set each frame.
    pub view_rect: Rect,
    /// Width / height of the rendered frame.
    pub frame_aspect: f32,
    pub quit: bool,
    copied_at: Option<f64>,
}

impl UiState {
    pub fn new(preset: StillnessMode, frame_aspect: f32) -> Self {
        Self {
            show_panels: true,
            show_meters: true,
            preset,
            view_rect: Rect::NOTHING,
            frame_aspect,
            quit: false,
            copied_at: None,
        }
    }
}

const ACCENT: Color32 = Color32::from_rgb(255, 143, 107);
const HISS: Color32 = Color32::from_rgb(127, 212, 255);

pub fn draw(ui: &mut egui::Ui, s: &mut Session) {
    keys(ui, s);
    if s.ui.show_panels {
        egui::Panel::bottom("transport").show(ui, |ui| transport_bar(ui, s));
        egui::Panel::right("controls").resizable(false).exact_size(290.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| controls(ui, s));
        });
    }
    egui::CentralPanel::no_frame().show(ui, |ui| {
        s.ui.view_rect = ui.max_rect();
        if s.ui.show_meters {
            meters(ui, s);
        }
    });
}

fn keys(ui: &mut egui::Ui, s: &mut Session) {
    if ui.ctx().egui_wants_keyboard_input() {
        return;
    }
    let (space, left, right, shift, home, l, i, o, h, m, esc) = ui.input(|i| {
        (
            i.key_pressed(Key::Space),
            i.key_pressed(Key::ArrowLeft),
            i.key_pressed(Key::ArrowRight),
            i.modifiers.shift,
            i.key_pressed(Key::Home),
            i.key_pressed(Key::L),
            i.key_pressed(Key::I),
            i.key_pressed(Key::O),
            i.key_pressed(Key::H),
            i.key_pressed(Key::M),
            i.key_pressed(Key::Escape),
        )
    });
    let t = &mut s.transport;
    let step = if shift { 1.0 / FPS as f64 } else { 5.0 };
    if space {
        t.toggle();
    }
    if left {
        t.seek(t.position() - step);
    }
    if right {
        t.seek(t.position() + step);
    }
    if home {
        t.seek(t.loop_range().0);
    }
    if l {
        t.looping = !t.looping;
    }
    if i {
        t.loop_in = Some(t.position());
    }
    if o {
        t.loop_out = Some(t.position());
    }
    if h {
        s.ui.show_panels = !s.ui.show_panels;
    }
    if m {
        s.ui.show_meters = !s.ui.show_meters;
    }
    if esc {
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
        ui.weak(if t.has_audio() { "audio" } else { "silent clock (no --audio)" });
    });
    scrub_strip(ui, s);
    ui.add_space(4.0);
}

/// Loudness envelope, click ticks, loop region and playhead. Click or drag
/// to seek.
fn scrub_strip(ui: &mut egui::Ui, s: &mut Session) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::click_and_drag());
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

    if let Some(f) = s.show.features() {
        let n = f.loud.len().max(1);
        let cols = rect.width().max(1.0) as usize;
        let (lane_top, lane_bottom) = (rect.top() + 14.0, rect.bottom() - 2.0);
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

    let px = x_of(s.transport.position());
    painter.line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(2.0, Color32::WHITE));

    if let Some(p) = response.interact_pointer_pos()
        && (response.clicked() || response.dragged())
    {
        s.transport.seek(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * duration);
    }
    response.on_hover_text("Loudness (grey), hiss (blue), clicks (orange). Click or drag to seek.");
}

fn controls(ui: &mut egui::Ui, s: &mut Session) {
    ui.add_space(6.0);
    ui.heading("Stillness");
    let look = &mut s.show.look;
    egui::ComboBox::from_id_salt("preset").selected_text(mode_name(s.ui.preset)).show_ui(ui, |ui| {
        for mode in [StillnessMode::StillAtRest, StillnessMode::AlwaysRestless, StillnessMode::Inverted] {
            if ui.selectable_value(&mut s.ui.preset, mode, mode_name(mode)).changed() {
                look.stillness = Stillness::preset(mode);
            }
        }
    });
    ui.add(Slider::new(&mut look.stillness.baseline, 0.0..=1.0).text("baseline"));
    ui.add(Slider::new(&mut look.stillness.response, 0.25..=4.0).logarithmic(true).text("response"));
    ui.checkbox(&mut look.stillness.invert, "invert (signal reveals the object)");

    ui.add_space(8.0);
    ui.heading("Snow mix");
    weights(ui, &mut look.snow);
    ui.add_space(8.0);
    ui.heading("Jitter mix");
    weights(ui, &mut look.jitter);
    ui.add_space(8.0);
    ui.heading("Tear mix");
    weights(ui, &mut look.tear);
    ui.add_space(8.0);
    ui.heading("Chroma split mix");
    weights(ui, &mut look.chroma);

    ui.add_space(8.0);
    ui.heading("Smoothing");
    let mut p = s.show.smoothing();
    let mut changed = envelope(ui, "hiss", &mut p.hiss);
    changed |= envelope(ui, "loud", &mut p.loud);
    changed |= ui.add(Slider::new(&mut p.click_decay, 0.02..=1.0).logarithmic(true).text("click decay s")).changed();
    if changed {
        s.show.set_smoothing(p);
    }

    ui.add_space(8.0);
    ui.heading("Snow");
    ui.add(Slider::new(&mut s.settings.snow_rows, 90.0..=1080.0).integer().text("rows"));
    ui.horizontal(|ui| {
        ui.label("seed");
        ui.add(egui::DragValue::new(&mut s.settings.seed));
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.heading("CRT");
        if ui.small_button("off").clicked() {
            s.settings.crt = Crt::OFF;
        }
        if ui.small_button("default").clicked() {
            s.settings.crt = Crt::default();
        }
    });
    let crt = &mut s.settings.crt;
    ui.add(Slider::new(&mut crt.scanlines, 0.0..=1.0).text("scanlines"));
    ui.add(Slider::new(&mut crt.vignette, 0.0..=1.0).text("vignette"));
    ui.add(Slider::new(&mut crt.bloom, 0.0..=1.0).text("bloom"));
    ui.add(Slider::new(&mut crt.persistence, 0.0..=0.95).text("persistence"));

    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui.button("Copy as TOML").on_hover_text("Copies these settings (also printed to the terminal)").clicked() {
            let toml = to_toml(s);
            println!("\n{toml}");
            ui.ctx().copy_text(toml);
            s.ui.copied_at = Some(ui.input(|i| i.time));
        }
        if s.ui.copied_at.is_some_and(|t| ui.input(|i| i.time) - t < 2.0) {
            ui.weak("copied");
        }
        if ui.button("Reset").clicked() {
            s.show.look = crate::look::Look::new(s.ui.preset);
            s.show.set_smoothing(Default::default());
            s.settings.crt = Crt::default();
        }
    });

    ui.add_space(12.0);
    ui.weak("Space play/pause · ←/→ 5 s (⇧ 1 frame) · Home loop start\nL loop · I/O loop in/out · H hide panels · M meters · Esc quit");
}

fn weights(ui: &mut egui::Ui, w: &mut Weights) {
    ui.add(Slider::new(&mut w.hiss, 0.0..=1.5).text("hiss"));
    ui.add(Slider::new(&mut w.click, 0.0..=1.5).text("click"));
    ui.add(Slider::new(&mut w.loud, 0.0..=1.5).text("loud"));
}

fn envelope(ui: &mut egui::Ui, name: &str, e: &mut Envelope) -> bool {
    let a = ui.add(Slider::new(&mut e.attack, 0.0..=2.0).text(format!("{name} attack s"))).changed();
    let r = ui.add(Slider::new(&mut e.release, 0.0..=4.0).text(format!("{name} release s"))).changed();
    a || r
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

/// Largest rect of the given aspect centred in `area` (matches the blit).
pub fn fit(area: Rect, aspect: f32) -> Rect {
    let size = if area.width() / area.height() > aspect {
        Vec2::new(area.height() * aspect, area.height())
    } else {
        Vec2::new(area.width(), area.width() / aspect)
    };
    Rect::from_center_size(area.center(), size)
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

/// The current settings in the shape `timeline.toml` cues will use.
fn to_toml(s: &Session) -> String {
    let look = &s.show.look;
    let p = s.show.smoothing();
    let st = &look.stillness;
    let ms = |secs: f32| format!("\"{}ms\"", (secs * 1000.0).round());
    let w = |w: &Weights| format!("{{ hiss = {:.2}, click = {:.2}, loud = {:.2} }}", w.hiss, w.click, w.loud);
    let mut out = String::new();
    let _ = writeln!(out, "# from `static preview` at {}", clock(s.transport.position()));
    let _ = writeln!(
        out,
        "stillness = {{ baseline = {:.2}, response = {:.2}, invert = {} }}",
        st.baseline, st.response, st.invert
    );
    let _ = writeln!(out, "snow_mix = {}", w(&look.snow));
    let _ = writeln!(out, "jitter_mix = {}", w(&look.jitter));
    let _ = writeln!(out, "tear_mix = {}", w(&look.tear));
    let _ = writeln!(out, "chroma_mix = {}", w(&look.chroma));
    let _ = writeln!(out, "hiss = {{ attack = {}, release = {} }}", ms(p.hiss.attack), ms(p.hiss.release));
    let _ = writeln!(out, "loud = {{ attack = {}, release = {} }}", ms(p.loud.attack), ms(p.loud.release));
    let _ = writeln!(out, "clicks = {{ decay = {} }}", ms(p.click_decay));
    let _ = writeln!(out, "snow = {{ rows = {} }}", s.settings.snow_rows as u32);
    let c = &s.settings.crt;
    let _ = writeln!(
        out,
        "crt = {{ scanlines = {:.2}, vignette = {:.2}, bloom = {:.2}, persistence = {:.2} }}",
        c.scanlines, c.vignette, c.bloom, c.persistence
    );
    let _ = write!(out, "# seed = {} (goes in [meta])", s.settings.seed);
    out
}
