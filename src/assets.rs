//! Loading and framing source images.
//!
//! Photos are cropped and resized once, on the CPU, to the exact output
//! resolution, so the shaders always sample them 1:1. Wherever a photo
//! doesn't cover the frame (a `scale` below 1, letterboxing, a transparent
//! PNG or SVG) its alpha is 0 and the background shows. Each photo also gets a
//! low-resolution signed distance to its outline, so the shaders can find its
//! border. The title is rasterised once into a full-frame coverage mask.

use std::path::Path;
use std::sync::Arc;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use anyhow::{Context, Result, anyhow};
use image::imageops::{self, FilterType};
use image::{GrayImage, Luma, Rgba, RgbaImage};

use crate::timeline::{Align, PhotoSpec, Timeline, TitleSpec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Fit {
    /// Crop to the output aspect ratio around the focus point.
    Fill,
    /// Fit the whole photo inside the frame, letterboxed on the background.
    Contain,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Framing {
    pub fit: Fit,
    /// Crop centre in normalised photo coordinates (0..1).
    pub focus: [f32; 2],
    /// Size of the box the photo is fitted to, as a fraction of the frame.
    pub scale: f32,
    /// Centre of that box, in normalised frame coordinates.
    pub position: [f32; 2],
}

impl Default for Framing {
    fn default() -> Self {
        Self { fit: Fit::Fill, focus: [0.5, 0.5], scale: 1.0, position: [0.5, 0.5] }
    }
}

/// Distance range the outline texture covers each side of the outline, in
/// frame heights. Must match `SHAPE_RANGE` in shaders/photo.wgsl.
pub const SHAPE_RANGE: f32 = 0.25;
/// The outline texture is this many times smaller than the frame.
pub const SHAPE_DOWNSAMPLE: u32 = 4;

pub fn load_photo(path: &Path, width: u32, height: u32, framing: &Framing) -> Result<RgbaImage> {
    let (bw, bh, _, _) = photo_box(width, height, framing);
    let is_svg = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"));
    let src = if is_svg {
        load_svg(path, bw, bh, framing.fit)?
    } else {
        image::open(path).with_context(|| format!("opening photo {}", path.display()))?.to_rgba8()
    };
    Ok(frame_photo(&src, width, height, framing, path))
}

/// Rasterises an SVG just large enough to fill or fit the box, so it stays
/// sharp at any output size.
fn load_svg(path: &Path, bw: u32, bh: u32, fit: Fit) -> Result<RgbaImage> {
    use resvg::{tiny_skia, usvg};
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut options = usvg::Options { resources_dir: path.parent().map(Path::to_path_buf), ..Default::default() };
    options.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_data(&data, &options).with_context(|| format!("parsing SVG {}", path.display()))?;
    let (sw, sh) = (tree.size().width(), tree.size().height());
    let (fx, fy) = (bw as f32 / sw, bh as f32 / sh);
    let k = match fit {
        Fit::Fill => fx.max(fy),
        Fit::Contain => fx.min(fy),
    };
    let (w, h) = (((sw * k).round() as u32).max(1), ((sh * k).round() as u32).max(1));
    let mut pixmap = tiny_skia::Pixmap::new(w, h).context("SVG too large")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(w as f32 / sw, h as f32 / sh), &mut pixmap.as_mut());
    let pixels = pixmap.pixels().iter().flat_map(|p| {
        let c = p.demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    });
    RgbaImage::from_raw(w, h, pixels.collect()).context("SVG size mismatch")
}

/// The box a photo is fitted into: size and top-left corner, in pixels.
fn photo_box(width: u32, height: u32, framing: &Framing) -> (u32, u32, i64, i64) {
    let scale = framing.scale.clamp(0.01, 1.0) as f64;
    let bw = ((width as f64 * scale).round() as u32).max(1);
    let bh = ((height as f64 * scale).round() as u32).max(1);
    let x = (framing.position[0] as f64 * width as f64 - bw as f64 / 2.0).round() as i64;
    let y = (framing.position[1] as f64 * height as f64 - bh as f64 / 2.0).round() as i64;
    (bw, bh, x, y)
}

fn frame_photo(src: &RgbaImage, width: u32, height: u32, framing: &Framing, path: &Path) -> RgbaImage {
    let (bw, bh, x, y) = photo_box(width, height, framing);
    let boxed = fit_photo(src, bw, bh, framing, path);
    if (bw, bh, x, y) == (width, height, 0, 0) {
        return boxed;
    }
    let mut canvas = RgbaImage::new(width, height);
    imageops::replace(&mut canvas, &boxed, x, y);
    canvas
}

/// The photo cropped (fill) or letterboxed (contain) to exactly `width` ×
/// `height`; letterbox bars are transparent.
fn fit_photo(src: &RgbaImage, width: u32, height: u32, framing: &Framing, path: &Path) -> RgbaImage {
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
            let mut canvas = RgbaImage::new(width, height);
            imageops::replace(&mut canvas, &resized, ((width - fw) / 2) as i64, ((height - fh) / 2) as i64);
            canvas
        }
    }
}

/// Signed distance from each point to the photo's outline (positive inside),
/// at 1/`SHAPE_DOWNSAMPLE` resolution, encoded 0..255 over ±`SHAPE_RANGE`
/// frame heights. Beyond the frame counts as outside, so a full-frame photo's
/// outline is the frame edge.
pub fn outline(photo: &RgbaImage) -> GrayImage {
    let (w, h) = photo.dimensions();
    let (sw, sh) = (w.div_ceil(SHAPE_DOWNSAMPLE), h.div_ceil(SHAPE_DOWNSAMPLE));
    // Coverage per small cell, with a one-cell border of outside.
    let (pw, ph) = (sw as usize + 2, sh as usize + 2);
    let mut inside = vec![false; pw * ph];
    for cy in 0..sh {
        for cx in 0..sw {
            let (mut sum, mut n) = (0u32, 0u32);
            for y in cy * SHAPE_DOWNSAMPLE..((cy + 1) * SHAPE_DOWNSAMPLE).min(h) {
                for x in cx * SHAPE_DOWNSAMPLE..((cx + 1) * SHAPE_DOWNSAMPLE).min(w) {
                    sum += photo.get_pixel(x, y).0[3] as u32;
                    n += 1;
                }
            }
            inside[(cy as usize + 1) * pw + cx as usize + 1] = sum * 2 >= n * 255;
        }
    }
    let to_inside = distance(&inside, pw, ph, true);
    let to_outside = distance(&inside, pw, ph, false);
    GrayImage::from_fn(sw, sh, |x, y| {
        let i = (y as usize + 1) * pw + x as usize + 1;
        let cells = if inside[i] { to_outside[i] - 0.5 } else { 0.5 - to_inside[i] };
        let d = cells / sh as f32;
        Luma([((0.5 + 0.5 * d / SHAPE_RANGE).clamp(0.0, 1.0) * 255.0).round() as u8])
    })
}

/// Euclidean distance from every cell to the nearest cell where
/// `mask == target` (Felzenszwalb & Huttenlocher's separable transform).
fn distance(mask: &[bool], w: usize, h: usize, target: bool) -> Vec<f32> {
    const FAR: f32 = 1e20;
    let mut d: Vec<f32> = mask.iter().map(|&m| if m == target { 0.0 } else { FAR }).collect();
    let mut line = vec![0.0; w.max(h)];
    let mut out = vec![0.0; w.max(h)];
    for x in 0..w {
        (0..h).for_each(|y| line[y] = d[y * w + x]);
        transform_1d(&line[..h], &mut out[..h]);
        (0..h).for_each(|y| d[y * w + x] = out[y]);
    }
    for y in 0..h {
        line[..w].copy_from_slice(&d[y * w..(y + 1) * w]);
        transform_1d(&line[..w], &mut out[..w]);
        d[y * w..(y + 1) * w].copy_from_slice(&out[..w]);
    }
    d.iter().map(|v| v.sqrt()).collect()
}

/// Squared distance transform of one line (lower envelope of parabolas).
fn transform_1d(f: &[f32], out: &mut [f32]) {
    let n = f.len();
    let mut v = vec![0usize; n];
    let mut z = vec![0.0f32; n + 1];
    let mut k = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    let meet = |q: usize, p: usize| ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2 * (q - p)) as f32;
    for q in 1..n {
        let mut s = meet(q, v[k]);
        while s <= z[k] {
            k -= 1;
            s = meet(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f32::INFINITY;
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        *o = (q as f32 - v[k] as f32).powi(2) + f[v[k]];
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

/// Everything the renderer uploads: one texture layer per timeline photo
/// (and its outline), and the title mask.
pub struct Assets {
    pub width: u32,
    pub height: u32,
    /// At least one layer (a black frame if the timeline has no photos).
    pub photos: Vec<Arc<RgbaImage>>,
    /// `outline` of each photo.
    pub outlines: Vec<Arc<GrayImage>>,
    pub title: Option<Arc<GrayImage>>,
}

impl Assets {
    /// Photos straight from memory (tests), outlines computed.
    #[cfg(test)]
    pub fn from_photos(photos: Vec<RgbaImage>) -> Self {
        let (width, height) = photos[0].dimensions();
        let outlines = photos.iter().map(|p| Arc::new(outline(p))).collect();
        Self { width, height, photos: photos.into_iter().map(Arc::new).collect(), outlines, title: None }
    }
}

/// Loads assets for a timeline, reusing whatever an earlier load already
/// prepared (the preview reloads on every timeline save).
pub struct AssetCache {
    width: u32,
    height: u32,
    photos: Vec<(PhotoSpec, Arc<RgbaImage>, Arc<GrayImage>)>,
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
        let (mut photos, mut outlines): (Vec<_>, Vec<_>) = timeline.photos.iter().map(|spec| self.photo(spec)).unzip();
        if photos.is_empty() {
            let black = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 255]));
            outlines.push(Arc::new(outline(&black)));
            photos.push(Arc::new(black));
        }
        let title = timeline.title.as_ref().and_then(|spec| self.title(spec));
        Assets { width, height, photos, outlines, title }
    }

    fn photo(&mut self, spec: &PhotoSpec) -> (Arc<RgbaImage>, Arc<GrayImage>) {
        if let Some((_, img, outline)) = self.photos.iter().find(|(s, _, _)| s == spec) {
            return (img.clone(), outline.clone());
        }
        let (w, h) = (self.width, self.height);
        let mut img = if spec.path.as_os_str().is_empty() {
            test_card(w, h)
        } else {
            load_photo(&spec.path, w, h, &spec.framing).unwrap_or_else(|e| {
                // Paths are relative to the timeline, so a bare file name
                // misses the photos folder.
                let in_photos = spec.path.parent().zip(spec.path.file_name()).map(|(d, n)| d.join("photos").join(n));
                let hint = match in_photos {
                    Some(p) if !spec.path.exists() && p.exists() => {
                        format!(
                            " (paths are relative to the timeline: did you mean \"photos/{}\"?)",
                            p.file_name().unwrap().to_string_lossy()
                        )
                    }
                    _ => String::new(),
                };
                eprintln!("warning: {e:#}{hint}; showing the test card instead");
                test_card(w, h)
            })
        };
        if spec.soften > 0.0 {
            // Up to a few pixels at 1080p, scaled with the output.
            img = imageops::fast_blur(&img, spec.soften * 3.0 * h as f32 / 1080.0);
        }
        // Every layer of the texture array has the frame's size.
        if img.dimensions() != (w, h) {
            img = imageops::resize(&img, w, h, FilterType::Nearest);
        }
        let shape = Arc::new(outline(&img));
        let img = Arc::new(img);
        self.photos.push((spec.clone(), img.clone(), shape.clone()));
        (img, shape)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(v: u8) -> f32 {
        (v as f32 / 255.0 - 0.5) * 2.0 * SHAPE_RANGE
    }

    #[test]
    fn outline_is_signed_distance_to_the_photo_edge() {
        // 400×200 frame, photo covering x 100..300, y 50..150.
        let photo = RgbaImage::from_fn(400, 200, |x, y| {
            let a = if (100..300).contains(&x) && (50..150).contains(&y) { 255 } else { 0 };
            Rgba([200, 200, 200, a])
        });
        let o = outline(&photo);
        assert_eq!(o.dimensions(), (100, 50));
        // Centre: 50 px (= 0.25 frame heights) from the top and bottom edges.
        let centre = decode(o.get_pixel(50, 25).0[0]);
        assert!((centre - 0.25).abs() < 0.02, "centre {centre}");
        // Just outside the left edge, and far outside.
        let near = decode(o.get_pixel(24, 25).0[0]);
        assert!((-0.03..0.0).contains(&near), "near {near}");
        assert!(decode(o.get_pixel(0, 0).0[0]) < -0.2);
    }

    #[test]
    fn full_frame_outline_is_the_frame_edge() {
        let o = outline(&RgbaImage::from_pixel(400, 200, Rgba([0, 0, 0, 255])));
        assert!((0.0..0.02).contains(&decode(o.get_pixel(0, 25).0[0])));
        assert!(decode(o.get_pixel(50, 25).0[0]) > 0.2);
    }

    #[test]
    fn scale_and_position_place_the_photo_on_transparency() {
        let src = RgbaImage::from_pixel(160, 90, Rgba([255, 255, 255, 255]));
        let framing = Framing { scale: 0.5, position: [0.25, 0.5], ..Default::default() };
        let f = frame_photo(&src, 320, 180, &framing, Path::new("test"));
        assert_eq!(f.get_pixel(80, 90).0[3], 255, "inside the box");
        assert_eq!(f.get_pixel(240, 90).0[3], 0, "outside it");
        assert_eq!(f.get_pixel(80, 10).0[3], 0, "above it");
    }
}
