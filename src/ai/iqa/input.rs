//! Decoded frame → 8-bit RGB, the input the Charm reference reads through
//! Pillow, except that EXIF orientation is applied: composition is judged
//! as the photo is displayed, where the reference sees stored pixels.
//!
//! Samples are taken as display-ready values: color profiles are ignored,
//! alpha is dropped and float data (linear by convention here) is
//! sRGB-encoded.

use super::resample::Rgb8;
use crate::domain::{ImageFrame, Orientation, Pixels};

const RGB: usize = 3;

pub fn display_rgb8(frame: &ImageFrame, orientation: Orientation) -> Rgb8 {
    let stored = match &frame.pixels {
        Pixels::U8(v) => to_rgb8(v, frame.has_alpha, |s| s),
        Pixels::U16(v) => to_rgb8(v, frame.has_alpha, |s| {
            ((u32::from(s) * 255 + u32::from(u16::MAX) / 2) / u32::from(u16::MAX)) as u8
        }),
        Pixels::F32(v) => to_rgb8(v, frame.has_alpha, |s| {
            (srgb_encode(s.clamp(0.0, 1.0)) * 255.0).round() as u8
        }),
    };
    orient(
        Rgb8 {
            width: frame.width as usize,
            height: frame.height as usize,
            data: stored,
        },
        orientation,
    )
}

fn to_rgb8<T: Copy>(src: &[T], has_alpha: bool, to_u8: impl Fn(T) -> u8) -> Vec<u8> {
    let stride = if has_alpha { RGB + 1 } else { RGB };
    src.chunks_exact(stride)
        .flat_map(|px| px[..RGB].iter().map(|&s| to_u8(s)))
        .collect()
}

/// The image as displayed: each display pixel taken from the stored pixel
/// the EXIF orientation maps onto it.
fn orient(src: Rgb8, orientation: Orientation) -> Rgb8 {
    let (w, h) = (src.width, src.height);
    let o = orientation.get();
    if !(2..=8).contains(&o) {
        return src;
    }
    let (dw, dh) = if o >= 5 { (h, w) } else { (w, h) };
    let mut data = Vec::with_capacity(src.data.len());
    for dy in 0..dh {
        for dx in 0..dw {
            let (sx, sy) = match o {
                2 => (w - 1 - dx, dy),
                3 => (w - 1 - dx, h - 1 - dy),
                4 => (dx, h - 1 - dy),
                5 => (dy, dx),
                6 => (dy, h - 1 - dx),
                7 => (w - 1 - dy, h - 1 - dx),
                _ => (w - 1 - dy, dx),
            };
            data.extend_from_slice(&src.data[(sy * w + sx) * RGB..][..RGB]);
        }
    }
    Rgb8 {
        width: dw,
        height: dh,
        data,
    }
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
    fn converts_depths_and_drops_alpha() {
        let f = frame(1, 1, Pixels::U8(vec![1, 2, 3, 4]), true);
        assert_eq!(display_rgb8(&f, Orientation::new(1)).data, [1, 2, 3]);
        let f = frame(1, 1, Pixels::U16(vec![u16::MAX, 0, 257 * 100]), false);
        assert_eq!(display_rgb8(&f, Orientation::new(1)).data, [255, 0, 100]);
        let f = frame(1, 1, Pixels::F32(vec![0.18, 2.0, -1.0]), false);
        assert_eq!(display_rgb8(&f, Orientation::new(1)).data, [118, 255, 0]);
    }

    #[test]
    fn orientation_6_rotates_clockwise_for_display() {
        // 3x2 stored, red marks the top-left pixel.
        let mut px = vec![0u8; 3 * 2 * RGB];
        px[0] = 255;
        let out = display_rgb8(&frame(3, 2, Pixels::U8(px), false), Orientation::new(6));
        assert_eq!((out.width, out.height), (2, 3));
        // Rotated 90° clockwise, stored top-left is displayed top-right.
        assert_eq!(out.data[RGB], 255);
        assert_eq!(out.data.iter().filter(|&&v| v == 255).count(), 1);
    }

    #[test]
    fn every_orientation_is_a_permutation() {
        let px: Vec<u8> = (0..3 * 2).flat_map(|i| [i as u8; RGB]).collect();
        for o in 1..=8 {
            let out = display_rgb8(
                &frame(3, 2, Pixels::U8(px.clone()), false),
                Orientation::new(o),
            );
            let mut seen: Vec<u8> = out.data.as_chunks::<RGB>().0.iter().map(|p| p[0]).collect();
            seen.sort_unstable();
            assert_eq!(seen, [0, 1, 2, 3, 4, 5], "orientation {o}");
        }
    }
}
