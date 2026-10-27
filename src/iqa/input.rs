//! Model input: the displayed image (orientation applied) area-resampled to
//! a `side`×`side` RGB square, planar, samples in 0..=1.
//!
//! Samples are taken as display-ready values: color profiles are ignored
//! and float data (linear by convention here) is sRGB-encoded.

use crate::domain::{ImageFrame, Orientation, Pixels};

const RGB: usize = 3;

/// Planar `[3, side, side]` tensor data.
pub fn prepare(frame: &ImageFrame, orientation: Orientation, side: usize) -> Vec<f32> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let channels = if frame.has_alpha { RGB + 1 } else { RGB };
    let square = match &frame.pixels {
        Pixels::U8(v) => resample(v, w, h, channels, side, |s| f32::from(s) / f32::from(u8::MAX)),
        Pixels::U16(v) => resample(v, w, h, channels, side, |s| f32::from(s) / f32::from(u16::MAX)),
        Pixels::F32(v) => resample(v, w, h, channels, side, |s| srgb_encode(s.clamp(0.0, 1.0))),
    };
    planar_oriented(&square, side, orientation)
}

/// Interleaved RGB `side`×`side`, each output pixel the area average of the
/// source region it covers (aspect ratio not preserved).
fn resample<T: Copy>(
    src: &[T],
    w: usize,
    h: usize,
    channels: usize,
    side: usize,
    to_unit: impl Fn(T) -> f32,
) -> Vec<f32> {
    let (cols, rows) = (area_weights(w, side), area_weights(h, side));
    let mut horizontal = vec![0f32; h * side * RGB];
    for y in 0..h {
        let row = &src[y * w * channels..(y + 1) * w * channels];
        for (ox, weights) in cols.iter().enumerate() {
            let out = &mut horizontal[(y * side + ox) * RGB..][..RGB];
            for &(x, weight) in weights {
                for (c, o) in out.iter_mut().enumerate() {
                    *o += to_unit(row[x * channels + c]) * weight;
                }
            }
        }
    }
    let mut square = vec![0f32; side * side * RGB];
    for (oy, weights) in rows.iter().enumerate() {
        for &(y, weight) in weights {
            let src_row = &horizontal[y * side * RGB..(y + 1) * side * RGB];
            let dst_row = &mut square[oy * side * RGB..(oy + 1) * side * RGB];
            for (d, s) in dst_row.iter_mut().zip(src_row) {
                *d += s * weight;
            }
        }
    }
    square
}

/// For each of `dst` output cells: the source indices it overlaps and the
/// overlap fraction (weights sum to 1).
fn area_weights(src: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|i| {
            let (start, end) = (i as f64 * scale, (i + 1) as f64 * scale);
            (start.floor() as usize..(end.ceil() as usize).min(src))
                .filter_map(|j| {
                    let overlap = end.min(j as f64 + 1.0) - start.max(j as f64);
                    (overlap > 0.0).then_some((j, (overlap / scale) as f32))
                })
                .collect()
        })
        .collect()
}

/// Interleaved stored-orientation square → planar display-orientation
/// square. A square keeps its size under all eight EXIF orientations, so
/// orienting after the resize equals resizing the displayed image.
fn planar_oriented(square: &[f32], side: usize, orientation: Orientation) -> Vec<f32> {
    let last = side - 1;
    let mut out = vec![0f32; RGB * side * side];
    for dy in 0..side {
        for dx in 0..side {
            // Stored pixel shown at display position (dx, dy).
            let (sx, sy) = match orientation.get() {
                2 => (last - dx, dy),
                3 => (last - dx, last - dy),
                4 => (dx, last - dy),
                5 => (dy, dx),
                6 => (dy, last - dx),
                7 => (last - dy, last - dx),
                8 => (last - dy, dx),
                _ => (dx, dy),
            };
            for c in 0..RGB {
                out[(c * side + dy) * side + dx] = square[(sy * side + sx) * RGB + c];
            }
        }
    }
    out
}

fn srgb_encode(linear: f32) -> f32 {
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, pixels: Pixels, has_alpha: bool) -> ImageFrame {
        ImageFrame {
            width,
            height,
            bit_depth: 8,
            has_alpha,
            alpha_premultiplied: false,
            pixels,
        }
    }

    #[test]
    fn area_weights_cover_each_cell_exactly() {
        for (src, dst) in [(10, 3), (3, 10), (384, 384), (4000, 384)] {
            for cell in area_weights(src, dst) {
                let sum: f32 = cell.iter().map(|(_, w)| w).sum();
                assert!((sum - 1.0).abs() < 1e-4, "{src}->{dst}: {sum}");
            }
        }
    }

    #[test]
    fn downscale_averages_and_drops_alpha() {
        // 2x2 RGBA: left column black, right column white; alpha ignored.
        let px = vec![0, 0, 0, 9, 255, 255, 255, 9, 0, 0, 0, 9, 255, 255, 255, 9];
        let out = prepare(&frame(2, 2, Pixels::U8(px), true), Orientation::new(1), 1);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|v| (v - 0.5).abs() < 1e-6));
    }

    #[test]
    fn output_is_planar_and_normalized() {
        let px = vec![u16::MAX, 0, u16::MAX / 2];
        let out = prepare(&frame(1, 1, Pixels::U16(px), false), Orientation::new(1), 2);
        assert_eq!(out[..4], [1.0; 4]);
        assert_eq!(out[4..8], [0.0; 4]);
        assert!(out[8..].iter().all(|v| (v - 0.5).abs() < 1e-4));
    }

    #[test]
    fn float_is_srgb_encoded() {
        let out = prepare(&frame(1, 1, Pixels::F32(vec![0.18, 2.0, -1.0]), false), Orientation::new(1), 1);
        for (got, want) in out.iter().zip([0.4613, 1.0, 0.0]) {
            assert!((got - want).abs() < 1e-3, "{got} vs {want}");
        }
    }

    #[test]
    fn orientation_6_rotates_clockwise_for_display() {
        // 2x2, red channel marks the stored top-left pixel only.
        let mut px = vec![0u8; 2 * 2 * 3];
        px[0] = 255;
        let out = prepare(&frame(2, 2, Pixels::U8(px), false), Orientation::new(6), 2);
        // Rotated 90° clockwise, stored top-left is displayed top-right.
        assert_eq!(out[..4], [0.0, 1.0, 0.0, 0.0]);
    }
}
