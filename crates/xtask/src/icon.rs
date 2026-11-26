//! Draws the app icon, an onigiri mascot with a `>_` terminal prompt, and packs
//! it into an `.icns` file. The icon is drawn in code so every size is sharp
//! and the repository holds no image files.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use image::{Rgba, RgbaImage};

type Vec2 = (f32, f32);

const fn rgb(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

// Warm off-white macOS icon tile with matte charcoal silhouette and vermilion dot.
const TILE_BORDER: [u8; 3] = rgb(0xe2dfd5);
const TILE_BG: [u8; 3] = rgb(0xf5f3ec);
const INK: [u8; 3] = rgb(0x26292e);
const ACCENT: [u8; 3] = rgb(0xe05242);

/// Samples per pixel along each axis, for smooth edges.
const SAMPLES: u32 = 4;

fn sd_rounded_rect(p: Vec2, left: f32, top: f32, right: f32, bottom: f32, radius: f32) -> f32 {
    let dx = (left + radius - p.0).max(p.0 - (right - radius));
    let dy = (top + radius - p.1).max(p.1 - (bottom - radius));
    dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - radius
}

fn sd_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let pa = (p.0 - a.0, p.1 - a.1);
    let ba = (b.0 - a.0, b.1 - a.1);
    let denom = ba.0 * ba.0 + ba.1 * ba.1;
    let h = if denom > 0.0 {
        ((pa.0 * ba.0 + pa.1 * ba.1) / denom).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (pa.0 - ba.0 * h).hypot(pa.1 - ba.1 * h)
}

fn sd_circle(p: Vec2, center: Vec2, radius: f32) -> f32 {
    (p.0 - center.0).hypot(p.1 - center.1) - radius
}

/// Exact signed distance to a triangle (negative inside, positive outside).
fn sd_triangle(p: Vec2, p0: Vec2, p1: Vec2, p2: Vec2) -> f32 {
    let e0 = (p1.0 - p0.0, p1.1 - p0.1);
    let e1 = (p2.0 - p1.0, p2.1 - p1.1);
    let e2 = (p0.0 - p2.0, p0.1 - p2.1);

    let v0 = (p.0 - p0.0, p.1 - p0.1);
    let v1 = (p.0 - p1.0, p.1 - p1.1);
    let v2 = (p.0 - p2.0, p.1 - p2.1);

    let dot = |a: Vec2, b: Vec2| a.0 * b.0 + a.1 * b.1;
    let cross = |a: Vec2, b: Vec2| a.0 * b.1 - a.1 * b.0;

    let clamp_proj = |v: Vec2, e: Vec2| {
        let t = (dot(v, e) / dot(e, e)).clamp(0.0, 1.0);
        let q = (v.0 - e.0 * t, v.1 - e.1 * t);
        dot(q, q)
    };

    let d_sq = clamp_proj(v0, e0)
        .min(clamp_proj(v1, e1))
        .min(clamp_proj(v2, e2));

    let s = cross(e0, e2).signum();
    let inside = (s * cross(v0, e0))
        .min(s * cross(v1, e1))
        .min(s * cross(v2, e2));

    let dist = d_sq.sqrt();
    if inside >= 0.0 { -dist } else { dist }
}

/// Evaluates the icon color at normalized coordinates `(x, y)` in `[0, 1]`.
fn sample_color(x: f32, y: f32) -> Option<[u8; 3]> {
    let p = (x, y);

    // Outer macOS icon tile (keeps standard ~10% margin).
    if sd_rounded_rect(p, 0.098, 0.098, 0.902, 0.902, 0.185) > 0.0 {
        return None;
    }
    let mut color = if sd_rounded_rect(p, 0.106, 0.106, 0.894, 0.894, 0.177) <= 0.0 {
        TILE_BG
    } else {
        TILE_BORDER
    };

    // Rounded-triangle onigiri silhouette.
    let onigiri = sd_triangle(p, (0.50, 0.345), (0.305, 0.665), (0.695, 0.665)) - 0.115;
    let stroke_half = 0.027;
    if onigiri.abs() <= stroke_half {
        color = INK;
    } else if onigiri < -stroke_half {
        // Nori wrap at the bottom center of the onigiri.
        if sd_rounded_rect(p, 0.382, 0.615, 0.618, 0.820, 0.032) <= 0.0 {
            color = INK;
        }
    }

    // `>_` terminal prompt centered in the onigiri.
    let chevron = sd_segment(p, (0.402, 0.415), (0.482, 0.475)).min(sd_segment(
        p,
        (0.482, 0.475),
        (0.402, 0.535),
    )) - 0.022;
    let underscore = sd_segment(p, (0.515, 0.538), (0.602, 0.538)) - 0.020;
    if chevron <= 0.0 || underscore <= 0.0 {
        color = INK;
    }

    // Active agent / umeboshi accent dot.
    if sd_circle(p, (0.612, 0.422), 0.026) <= 0.0 {
        color = ACCENT;
    }

    Some(color)
}

/// Draws the icon as a square of `size` pixels.
pub fn render(size: u32) -> RgbaImage {
    let mut image = RgbaImage::new(size, size);
    let step = 1.0 / (size * SAMPLES) as f32;
    let samples = (SAMPLES * SAMPLES) as f32;
    for (px, py, pixel) in image.enumerate_pixels_mut() {
        let mut color = [0.0f32; 3];
        let mut covered = 0.0f32;
        for sy in 0..SAMPLES {
            for sx in 0..SAMPLES {
                let x = (px * SAMPLES + sx) as f32 * step + step / 2.0;
                let y = (py * SAMPLES + sy) as f32 * step + step / 2.0;
                if let Some(sample) = sample_color(x, y) {
                    for (sum, value) in color.iter_mut().zip(sample) {
                        *sum += value as f32;
                    }
                    covered += 1.0;
                }
            }
        }
        // Straight alpha: the color is the average of the covered samples.
        let channel = |sum: f32| {
            if covered == 0.0 {
                0
            } else {
                (sum / covered).round() as u8
            }
        };
        *pixel = Rgba([
            channel(color[0]),
            channel(color[1]),
            channel(color[2]),
            (covered / samples * 255.0).round() as u8,
        ]);
    }
    image
}

/// The images of an iconset, as point size and scale.
const ICONSET: &[(u32, u32)] = &[
    (16, 1),
    (16, 2),
    (32, 1),
    (32, 2),
    (128, 1),
    (128, 2),
    (256, 1),
    (256, 2),
    (512, 1),
    (512, 2),
];

/// Writes the icon at every size macOS asks for to `out` as an `.icns` file.
pub fn write_icns(out: &Path) -> Result<()> {
    let iconset = out.with_extension("iconset");
    if iconset.exists() {
        fs::remove_dir_all(&iconset).with_context(|| format!("remove {}", iconset.display()))?;
    }
    fs::create_dir_all(&iconset).with_context(|| format!("create {}", iconset.display()))?;
    for &(points, scale) in ICONSET {
        let suffix = if scale == 1 { "" } else { "@2x" };
        let name = format!("icon_{points}x{points}{suffix}.png");
        render(points * scale)
            .save(iconset.join(&name))
            .with_context(|| format!("write {name}"))?;
    }
    let status = Command::new("iconutil")
        .args(["--convert", "icns", "--output"])
        .arg(out)
        .arg(&iconset)
        .status()
        .context("run iconutil")?;
    fs::remove_dir_all(&iconset).with_context(|| format!("remove {}", iconset.display()))?;
    if !status.success() {
        bail!("iconutil could not pack {}", iconset.display());
    }
    Ok(())
}
