//! RGB -> full-range 10-bit Y'CbCr 4:2:0 (the only layout Windows' AV1
//! decoder handles: 4:4:4 needs AV1 High profile and decodes broken there).

use super::color::Rgb16;

const MAX_10: f32 = 1023.0;
const CHROMA_ZERO: u16 = 512;

/// Planar 10-bit 4:2:0 picture.
#[derive(Debug, Clone, PartialEq)]
pub struct Planes {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u16>,
    pub u: Vec<u16>,
    pub v: Vec<u16>,
}

fn luma_weights(matrix: u8) -> (f32, f32) {
    match matrix {
        1 => (0.2126, 0.0722),
        9 => (0.2627, 0.0593),
        _ => (0.299, 0.114), // 6: BT.601
    }
}

/// `v.round().clamp(0.0, MAX_10)` without `round`, which the baseline
/// x86-64 target compiles to a libm call: below 2^23 the fraction of a
/// clamped, non-negative sample is exact.
fn round_10(v: f32) -> u16 {
    let v = v.clamp(0.0, MAX_10);
    let whole = v as u16;
    whole + u16::from(v - f32::from(whole) >= 0.5)
}

fn q(v: f32) -> u16 {
    round_10(v * MAX_10)
}

fn q_chroma(sum: f32, n: u8) -> u16 {
    round_10(sum / f32::from(n) * MAX_10 + f32::from(CHROMA_ZERO))
}

/// Color planes; chroma is the mean of each 2x2 block (edge blocks of odd
/// sizes average the samples they have). H.273 full range: Y = 1023*E'y,
/// Cb = 1023*E'pb + 512. Bands of rows run on up to `threads` threads.
pub fn from_rgb(img: &Rgb16, matrix: u8, threads: usize) -> Planes {
    let (w, h) = (img.width as usize, img.height as usize);
    if w == 0 || h == 0 {
        return gray_planes(img.width, img.height, Vec::new());
    }
    let ch = img.channels();
    let (kr, kb) = luma_weights(matrix);
    let kg = 1.0 - kr - kb;
    let (cb_scale, cr_scale) = (0.5 / (1.0 - kb), 0.5 / (1.0 - kr));
    let (cw, chh) = (w.div_ceil(2), h.div_ceil(2));
    let mut y = vec![0u16; w * h];
    let mut u = vec![0u16; cw * chh];
    let mut v = vec![0u16; cw * chh];
    const NORM: f32 = 1.0 / 65535.0;

    // One chroma row: its (up to) two luma rows, block by block. Each block
    // sums its samples in the same order as a row-major scan.
    let chroma_row = |rgb: &[u16], y: &mut [u16], u: &mut [u16], v: &mut [u16]| {
        let rows = y.len() / w;
        for (cx, (u, v)) in u.iter_mut().zip(v.iter_mut()).enumerate() {
            let (mut cb, mut cr, mut n) = (0f32, 0f32, 0u8);
            for row in 0..rows {
                for col in 2 * cx..(2 * cx + 2).min(w) {
                    let p = &rgb[(row * w + col) * ch..][..3];
                    let (r, g, b) = (p[0] as f32 * NORM, p[1] as f32 * NORM, p[2] as f32 * NORM);
                    let luma = kr * r + kg * g + kb * b;
                    y[row * w + col] = q(luma);
                    cb += (b - luma) * cb_scale;
                    cr += (r - luma) * cr_scale;
                    n += 1;
                }
            }
            *u = q_chroma(cb, n);
            *v = q_chroma(cr, n);
        }
    };

    let band = chh.div_ceil(threads.max(1)).max(1);
    std::thread::scope(|s| {
        let bands = img
            .data
            .chunks(band * 2 * w * ch)
            .zip(y.chunks_mut(band * 2 * w))
            .zip(u.chunks_mut(band * cw).zip(v.chunks_mut(band * cw)));
        for ((rgb, y), (u, v)) in bands {
            s.spawn(move || {
                let rows = rgb
                    .chunks(2 * w * ch)
                    .zip(y.chunks_mut(2 * w))
                    .zip(u.chunks_mut(cw).zip(v.chunks_mut(cw)));
                for ((rgb, y), (u, v)) in rows {
                    chroma_row(rgb, y, u, v);
                }
            });
        }
    });
    Planes {
        width: img.width,
        height: img.height,
        y,
        u,
        v,
    }
}

/// Alpha as the luma of a 4:2:0 picture with neutral chroma (SVT-AV1 has
/// no 4:0:0 mode; readers use only the Y plane of an alpha item).
pub fn from_alpha(img: &Rgb16) -> Planes {
    let y = img
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| ((u32::from(p[3]) * 1023 + 32767) / 65535) as u16)
        .collect();
    gray_planes(img.width, img.height, y)
}

/// 8-bit gray (a gain map) widened to 10 bits.
pub fn from_gray8(width: u32, height: u32, gray: &[u8]) -> Planes {
    let y = gray
        .iter()
        .map(|&v| ((u32::from(v) * 1023 + 127) / 255) as u16)
        .collect();
    gray_planes(width, height, y)
}

fn gray_planes(width: u32, height: u32, y: Vec<u16>) -> Planes {
    let n = width.div_ceil(2) as usize * height.div_ceil(2) as usize;
    Planes {
        width,
        height,
        y,
        u: vec![CHROMA_ZERO; n],
        v: vec![CHROMA_ZERO; n],
    }
}

impl Planes {
    /// `tw` x `th` window at (`x0`, `y0`), both even; samples past the
    /// picture edge repeat the last row/column.
    pub fn tile(&self, x0: u32, y0: u32, tw: u32, th: u32) -> Planes {
        debug_assert!([x0, y0, tw, th].iter().all(|v| v.is_multiple_of(2)));
        let crop = |plane: &[u16], pw: u32, ph: u32, x0: u32, y0: u32, tw: u32, th: u32| {
            let mut out = Vec::with_capacity((tw * th) as usize);
            for ty in 0..th {
                let sy = (y0 + ty).min(ph - 1) as usize;
                let row = &plane[sy * pw as usize..(sy + 1) * pw as usize];
                out.extend((0..tw).map(|tx| row[(x0 + tx).min(pw - 1) as usize]));
            }
            out
        };
        let (cw, ch) = (self.width.div_ceil(2), self.height.div_ceil(2));
        Planes {
            width: tw,
            height: th,
            y: crop(&self.y, self.width, self.height, x0, y0, tw, th),
            u: crop(&self.u, cw, ch, x0 / 2, y0 / 2, tw / 2, th / 2),
            v: crop(&self.v, cw, ch, x0 / 2, y0 / 2, tw / 2, th / 2),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(w: u32, h: u32, px: [u16; 3]) -> Rgb16 {
        Rgb16 {
            width: w,
            height: h,
            alpha: false,
            data: px.repeat((w * h) as usize),
        }
    }

    #[test]
    fn white_gray_and_red_bt601() {
        let p = from_rgb(&rgb(3, 3, [65535; 3]), 6, 1);
        assert!(p.y.iter().all(|&v| v == 1023));
        assert!(p.u.iter().chain(&p.v).all(|&v| v == 512));
        assert_eq!((p.u.len(), p.v.len()), (4, 4));
        let red = from_rgb(&rgb(2, 2, [65535, 0, 0]), 6, 1);
        assert_eq!(red.y[0], q(0.299));
        assert_eq!(red.v[0], 1023); // Cr = 0.5 -> clamped top
        assert_eq!(red.u[0], (512.0 - 0.299 / 1.772 * 1023.0f32).round() as u16);
    }

    #[test]
    fn round_10_matches_round_then_clamp() {
        let mut v = -2.0f32;
        while v < MAX_10 + 2.0 {
            for x in [
                v,
                v.next_down(),
                v.next_up(),
                v + 0.5,
                (v + 0.5).next_down(),
            ] {
                assert_eq!(round_10(x), x.round().clamp(0.0, MAX_10) as u16, "{x}");
            }
            v += 0.25;
        }
        assert_eq!(round_10(f32::NAN), 0);
    }

    #[test]
    fn bands_match_a_single_thread() {
        let (w, h) = (7, 9);
        for alpha in [false, true] {
            let ch = if alpha { 4 } else { 3 };
            let data = (0..w * h * ch).map(|i| (i * 7919 % 65536) as u16).collect();
            let img = Rgb16 {
                width: w,
                height: h,
                alpha,
                data,
            };
            let one = from_rgb(&img, 6, 1);
            for threads in [2, 3, 16] {
                assert_eq!(from_rgb(&img, 6, threads), one, "{threads} threads");
            }
        }
        assert!(from_rgb(&rgb(0, 3, [0; 3]), 6, 4).y.is_empty());
    }

    #[test]
    fn tile_replicates_edges() {
        let mut img = rgb(3, 1, [0; 3]);
        img.data[6..9].copy_from_slice(&[65535; 3]);
        let p = from_rgb(&img, 6, 1);
        let t = p.tile(2, 0, 4, 2);
        assert_eq!(t.y, [1023, 1023, 1023, 1023, 1023, 1023, 1023, 1023]);
        assert_eq!(t.u.len(), 2);
    }

    #[test]
    fn alpha_scales_to_10_bit() {
        let img = Rgb16 {
            width: 2,
            height: 1,
            alpha: true,
            data: vec![0, 0, 0, 65535, 0, 0, 0, 0],
        };
        let p = from_alpha(&img);
        assert_eq!(p.y, [1023, 0]);
        assert_eq!(p.u, [512]);
    }
}
