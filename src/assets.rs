//! Loading and framing source images.
//!
//! Photos are cropped and resized once, on the CPU, to the exact output
//! resolution, so the shaders always sample them 1:1.

use std::path::Path;

use anyhow::{Context, Result};
use image::imageops::{self, FilterType};
use image::{Rgba, RgbaImage};

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Fit {
    /// Crop to the output aspect ratio around the focus point.
    Fill,
    /// Fit the whole photo inside the frame, letterboxed on black.
    Contain,
}

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
