// SPDX-License-Identifier: GPL-3.0-only
//! Independent per-column light analysis and output rendering.
use crate::Palette;
use anyhow::{bail, Result};
use image::{GenericImageView, ImageReader};
use std::{fs, os::unix::fs::OpenOptionsExt, path::Path};
pub(crate) fn palette(path: &Path) -> Result<Palette> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        bail!("wallpaper must be a regular file")
    }
    if file.metadata()?.len() > 128 * 1024 * 1024 {
        bail!("wallpaper exceeds 128 MiB")
    }
    let mut reader = ImageReader::new(std::io::BufReader::new(file)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    use image::ImageDecoder;
    if decoder.total_bytes() > 128 * 1024 * 1024 {
        bail!("decoded wallpaper exceeds 128 MiB")
    }
    let orientation = decoder.orientation()?;
    let mut img = image::DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    let small = img.resize_exact(128, 64, image::imageops::FilterType::Nearest);
    drop(img);
    Ok(sample_palette(&small))
}
pub(crate) fn linear(v: u8) -> f64 {
    let x = f64::from(v) / 255.0;
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}
pub(crate) fn encoded(v: f64) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let x = if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (x * 255.0).round() as u8
}
pub(crate) fn luminance(rgb: [f64; 3]) -> f64 {
    rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722
}
pub(crate) fn contrast_weight(low: f64, high: f64) -> f64 {
    let difference = (high - low).max(0.0);
    // Absolute linear-light contrast: a 0.10 gap gives 50% highlight weight.
    // Unlike relative contrast, this does not amplify near-black noise.
    difference / (difference + 0.10)
}
pub(crate) fn enhance(rgb: [u8; 3]) -> [u8; 3] {
    let values = rgb.map(|v| f64::from(v) / 255.0);
    let max = values.iter().copied().fold(0.0, f64::max);
    let min = values.iter().copied().fold(1.0, f64::min);
    // Treat the darkest 6% of encoded value as black. Use a neutral
    // minimum rather than switching a zone off or retaining a colored tint.
    if max <= 0.06 {
        return [1; 3];
    }
    // Double HSV saturation, bounded at full saturation with a common gain
    // so channels retain the original hue rather than clipping separately.
    let saturation_gain = if max == min {
        1.0
    } else {
        2.0f64.min(max / (max - min))
    };
    // Smooth contrast curve with a fixed black point and white point.
    // Fixed points preserve relative brightness without per-image stretching.
    let t = ((max - 0.06) / (0.90 - 0.06)).clamp(0.0, 1.0);
    let value = t * t * (3.0 - 2.0 * t);
    if value <= 1.0 / 255.0 {
        return [1; 3];
    }
    values.map(|v| ((max - (max - v) * saturation_gain) / max * value * 255.0).round() as u8)
}
pub(crate) fn sample_palette(small: &image::DynamicImage) -> Palette {
    // A lookup table avoids three powf operations per sampled pixel.
    let lut: [f64; 256] = std::array::from_fn(|i| linear(i as u8));
    let mut out = [[0; 3]; 4];
    for (col, color) in out.iter_mut().enumerate() {
        let mut rgb_sum = [0.0f64; 3];
        let mut samples = Vec::with_capacity(2048);
        for y in 0..64 {
            for x in col as u32 * 32..(col as u32 + 1) * 32 {
                let p = small.get_pixel(x, y).0;
                let alpha = f64::from(p[3]) / 255.0;
                let rgb = [
                    lut[p[0] as usize] * alpha,
                    lut[p[1] as usize] * alpha,
                    lut[p[2] as usize] * alpha,
                ];
                samples.push((luminance(rgb), rgb));
                for i in 0..3 {
                    rgb_sum[i] += rgb[i];
                }
            }
        }
        // Tail means are robust to a few isolated bright/dark pixels.
        // Two linear-time selections avoid sorting the entire sample.
        let count = samples.len();
        let tail = (count * 5).div_ceil(100);
        samples.select_nth_unstable_by(tail, |a, b| a.0.total_cmp(&b.0));
        let low = samples[..tail].iter().map(|v| v.0).sum::<f64>() / tail as f64;
        let top_start = count - tail;
        samples.select_nth_unstable_by(top_start, |a, b| a.0.total_cmp(&b.0));
        let mut bright_rgb = [0.0f64; 3];
        let mut high = 0.0;
        for (level, rgb) in &samples[top_start..] {
            high += level;
            for i in 0..3 {
                bright_rgb[i] += rgb[i];
            }
        }
        high /= tail as f64;
        let weight = contrast_weight(low, high);
        for i in 0..3 {
            let average = rgb_sum[i] / count as f64;
            let highlight = bright_rgb[i] / tail as f64;
            // A convex blend changes both hue and intensity, remains in gamut,
            // and never normalizes a dim column to full brightness.
            color[i] = encoded(average * (1.0 - weight) + highlight * weight);
        }
    }
    out.map(enhance)
}
