//! Loading and framing source images.
//!
//! Photos are cropped and resized once, on the CPU, to the exact output
//! resolution, so the shaders always sample them 1:1. The title is
//! rasterised once into a full-frame coverage mask.

use std::path::Path;
use std::sync::Arc;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use anyhow::{Context, Result, anyhow};
use image::imageops::{self, FilterType};
use image::{GrayImage, Rgba, RgbaImage};

use crate::timeline::{Align, PhotoSpec, Timeline, TitleSpec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Fit {
    /// Crop to the output aspect ratio around the focus point.
    Fill,
    /// Fit the whole photo inside the frame, letterboxed on black.
    Contain,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Framing {
    pub fit: Fit,
    /// Crop centre in normalised photo coordinates (0..1).
    pub focus: [f32; 2],
}

pub fn load_photo(path: &Path, width: u32, height: u32, framing: &Framing) -> Result<RgbaImage> {
    let src = image::open(path).with_context(|| format!("opening photo {}", path.display()))?.to_rgba8();
    Ok(frame_photo(&src, width, height, framing, path))
}

fn frame_photo(src: &RgbaImage, width: u32, height: u32, framing: &Framing, path: &Path) -> RgbaImage {
    let (sw, sh) = src.dimensions();
    match framing.fit {
        Fit::Fill => {
            let aspect = width as f64 / height as f64;
            let (cw, ch) = if sw as f64 / sh as f64 > aspect {
                (((sh as f64 * aspect).round() as u32).min(sw), sh)
            } else {
                (sw, ((sw as f64 / aspect).round() as u32).min(sh))
            };
            let cx = (framing.focus[0] as f64 * sw as f64 - cw as f64 / 2.0).clamp(0.0, (sw - cw) as f64);
            let cy = (framing.focus[1] as f64 * sh as f64 - ch as f64 / 2.0).clamp(0.0, (sh - ch) as f64);
            let crop = imageops::crop_imm(src, cx as u32, cy as u32, cw, ch).to_image();
            warn_if_upscaled(path, width as f64 / cw as f64, width, height);
            if (cw, ch) == (width, height) {
                crop
            } else {
                imageops::resize(&crop, width, height, FilterType::Lanczos3)
            }
        }
        Fit::Contain => {
            let scale = (width as f64 / sw as f64).min(height as f64 / sh as f64);
            let fw = ((sw as f64 * scale).round() as u32).clamp(1, width);
            let fh = ((sh as f64 * scale).round() as u32).clamp(1, height);
            warn_if_upscaled(path, scale, width, height);
            let resized = imageops::resize(src, fw, fh, FilterType::Lanczos3);
            let mut canvas = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 255]));
            imageops::overlay(&mut canvas, &resized, ((width - fw) / 2) as i64, ((height - fh) / 2) as i64);
            canvas
        }
    }
}

fn warn_if_upscaled(path: &Path, scale: f64, width: u32, height: u32) {
    if scale > 1.0 + 1e-6 {
        eprintln!(
            "warning: {} is upscaled {scale:.2}× to {width}×{height}{}",
            path.display(),
            if scale > 1.5 { " (it may look soft; a higher-resolution photo would be sharper)" } else { "" }
        );
    }
}

/// A neutral placeholder for when no photo is given: grid, grey ramp and a
/// circle, which make scaling, cropping and colour handling easy to check.
pub fn test_card(width: u32, height: u32) -> RgbaImage {
    let (w, h) = (width as f32, height as f32);
    let cell = h / 12.0;
    let line = (cell * 0.02).max(1.0);
    RgbaImage::from_fn(width, height, |x, y| {
        let (fx, fy) = (x as f32, y as f32);
        let (u, v) = (fx / w, fy / h);
        let mut c = 0.06 + 0.08 * (1.0 - v);
        if fx % cell < line || fy % cell < line {
            c = 0.28;
        }
        if (0.44..0.56).contains(&v) {
            c = (u * 8.0).floor() / 7.0;
        }
        let r = ((fx - w / 2.0).powi(2) + (fy - h / 2.0).powi(2)).sqrt();
        if (r - h * 0.4).abs() < line * 1.5 {
            c = 0.85;
        }
        let g = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        Rgba([g, g, g, 255])
    })
}

/// Everything the renderer uploads: one texture layer per timeline photo,
/// and the title mask.
pub struct Assets {
    pub width: u32,
    pub height: u32,
    /// At least one layer (a black frame if the timeline has no photos).
    pub photos: Vec<Arc<RgbaImage>>,
    pub title: Option<Arc<GrayImage>>,
}

/// Loads assets for a timeline, reusing whatever an earlier load already
/// prepared (the preview reloads on every timeline save).
pub struct AssetCache {
    width: u32,
    height: u32,
    photos: Vec<(PhotoSpec, Arc<RgbaImage>)>,
    title: Option<(TitleSpec, Arc<GrayImage>)>,
}

impl AssetCache {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, photos: Vec::new(), title: None }
    }

    /// Missing or broken files are warnings, not errors, so a half-finished
    /// timeline still previews: a photo falls back to the test card, a title
    /// to nothing.
    pub fn assets(&mut self, timeline: &Timeline) -> Assets {
        let (width, height) = (self.width, self.height);
        let mut photos: Vec<_> = timeline.photos.iter().map(|spec| self.photo(spec)).collect();
        if photos.is_empty() {
            photos.push(Arc::new(RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 255]))));
        }
        // Layers of a texture array share one size.
        for p in &mut photos {
            if p.dimensions() != (width, height) {
                *p = Arc::new(imageops::resize(&**p, width, height, FilterType::Nearest));
            }
        }
        let title = timeline.title.as_ref().and_then(|spec| self.title(spec));
        Assets { width, height, photos, title }
    }

    fn photo(&mut self, spec: &PhotoSpec) -> Arc<RgbaImage> {
        if let Some((_, img)) = self.photos.iter().find(|(s, _)| s == spec) {
            return img.clone();
        }
        let (w, h) = (self.width, self.height);
        let mut img = if spec.path.as_os_str().is_empty() {
            test_card(w, h)
        } else {
            load_photo(&spec.path, w, h, &spec.framing).unwrap_or_else(|e| {
                eprintln!("warning: {e:#}; showing the test card instead");
                test_card(w, h)
            })
        };
        if spec.soften > 0.0 {
            // Up to a few pixels at 1080p, scaled with the output.
            img = imageops::fast_blur(&img, spec.soften * 3.0 * h as f32 / 1080.0);
        }
        let img = Arc::new(img);
        self.photos.push((spec.clone(), img.clone()));
        img
    }

    fn title(&mut self, spec: &TitleSpec) -> Option<Arc<GrayImage>> {
        if let Some((s, img)) = &self.title
            && s == spec
        {
            return Some(img.clone());
        }
        match render_title(spec, self.width, self.height) {
            Ok(img) => {
                let img = Arc::new(img);
                self.title = Some((spec.clone(), img.clone()));
                Some(img)
            }
            Err(e) => {
                eprintln!("warning: {e:#}; no title");
                None
            }
        }
    }
}

/// The title (and subtitle) as a full-frame coverage mask.
pub fn render_title(spec: &TitleSpec, width: u32, height: u32) -> Result<GrayImage> {
    let data = std::fs::read(&spec.font).with_context(|| format!("reading title font {}", spec.font.display()))?;
    let font = FontVec::try_from_vec_and_index(data, 0)
        .map_err(|e| anyhow!("{} is not a usable font: {e}", spec.font.display()))?;
    let h = height as f32;
    // Sizes are cap heights, so measure a capital.
    let cap_units = font
        .outline(font.glyph_id('H'))
        .map(|o| o.bounds.max.y - o.bounds.min.y)
        .filter(|&c| c > 0.0)
        .unwrap_or(font.height_unscaled() * 0.7);
    let scale = |cap: f32| PxScale::from(cap * font.height_unscaled() / cap_units);

    let (cap, sub_cap) = (spec.size * h, spec.subtitle_size * h);
    let has_sub = !spec.subtitle.trim().is_empty();
    let gap = cap * 1.2;
    let block = cap + if has_sub { gap + sub_cap } else { 0.0 };
    let top = spec.position[1] * h - block / 2.0;

    let mut img = GrayImage::new(width, height);
    draw_line(&font, &mut img, &spec.text, scale(cap), top + cap, spec);
    if has_sub {
        draw_line(&font, &mut img, &spec.subtitle, scale(sub_cap), top + cap + gap + sub_cap, spec);
    }
    Ok(img)
}

fn draw_line(font: &FontVec, img: &mut GrayImage, text: &str, scale: PxScale, baseline: f32, spec: &TitleSpec) {
    let scaled = font.as_scaled(scale);
    let em = scale.y * font.units_per_em().unwrap_or(1000.0) / font.height_unscaled();
    let spacing = spec.letter_spacing * em;
    let mut glyphs = Vec::new();
    let (mut x, mut prev) = (0.0, None);
    for c in text.chars() {
        let id = font.glyph_id(c);
        if let Some(p) = prev {
            x += scaled.kern(p, id);
        }
        glyphs.push((id, x));
        x += scaled.h_advance(id) + spacing;
        prev = Some(id);
    }
    let line_width = (x - spacing).max(0.0);
    let anchor = spec.position[0] * img.width() as f32;
    let left = match spec.align {
        Align::Left => anchor,
        Align::Center => anchor - line_width / 2.0,
        Align::Right => anchor - line_width,
    };
    let (w, h) = (img.width() as i32, img.height() as i32);
    for (id, gx) in glyphs {
        let Some(outline) = font.outline_glyph(id.with_scale_and_position(scale, point(left + gx, baseline))) else {
            continue;
        };
        let b = outline.px_bounds();
        outline.draw(|x, y, coverage| {
            let (px, py) = (b.min.x as i32 + x as i32, b.min.y as i32 + y as i32);
            if (0..w).contains(&px) && (0..h).contains(&py) {
                let p = img.get_pixel_mut(px as u32, py as u32);
                p.0[0] = p.0[0].max((coverage.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        });
    }
}
