//! The two bicubic resamplers the Charm reference relies on: Pillow's
//! `Image.resize` (initial downscale, 8-bit fixed point, antialiased) and
//! PyTorch's `interpolate(mode="bicubic")` (patches and position tables).
//! Both are reproduced exactly; the checkpoints were trained on their output.

/// Pillow's bicubic `a`.
const PIL_A: f64 = -0.5;
const PIL_SUPPORT: f64 = 2.0;
/// Pillow's fixed-point precision for 8-bit images (`32 - 8 - 2`).
const PRECISION_BITS: u32 = 22;
/// PyTorch's bicubic `a`.
const TORCH_A: f64 = -0.75;

/// Interleaved RGB 8-bit image.
pub struct Rgb8 {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

const RGB: usize = 3;

/// `Image.resize((width, height))` with Pillow's default bicubic filter.
pub fn pil_resize(src: &Rgb8, width: usize, height: usize) -> Rgb8 {
    let (xbounds, xk, xksize) = pil_coeffs(src.width, width);
    let mut horizontal = vec![0u8; src.height * width * RGB];
    for y in 0..src.height {
        let row = &src.data[y * src.width * RGB..][..src.width * RGB];
        for (x, &(xmin, count)) in xbounds.iter().enumerate() {
            let k = &xk[x * xksize..][..count];
            for c in 0..RGB {
                let sum = k
                    .iter()
                    .enumerate()
                    .map(|(i, &w)| i64::from(row[(xmin + i) * RGB + c]) * w)
                    .sum::<i64>();
                horizontal[(y * width + x) * RGB + c] = clip8(sum);
            }
        }
    }
    let (ybounds, yk, yksize) = pil_coeffs(src.height, height);
    let mut data = vec![0u8; height * width * RGB];
    for (y, &(ymin, count)) in ybounds.iter().enumerate() {
        let k = &yk[y * yksize..][..count];
        for i in 0..width * RGB {
            let sum = k
                .iter()
                .enumerate()
                .map(|(j, &w)| i64::from(horizontal[(ymin + j) * width * RGB + i]) * w)
                .sum::<i64>();
            data[y * width * RGB + i] = clip8(sum);
        }
    }
    Rgb8 {
        width,
        height,
        data,
    }
}

/// Rounding offset added before the fixed-point shift, then clamped.
fn clip8(sum: i64) -> u8 {
    ((sum + (1 << (PRECISION_BITS - 1))) >> PRECISION_BITS).clamp(0, 255) as u8
}

fn pil_filter(x: f64) -> f64 {
    let x = x.abs();
    if x < 1.0 {
        ((PIL_A + 2.0) * x - (PIL_A + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        (((x - 5.0) * x + 8.0) * x - 4.0) * PIL_A
    } else {
        0.0
    }
}

/// Pillow's `precompute_coeffs` + `normalize_coeffs_8bpc`: per output
/// pixel the first source index and tap count, and fixed-point weights in
/// rows of `ksize`.
fn pil_coeffs(in_size: usize, out_size: usize) -> (Vec<(usize, usize)>, Vec<i64>, usize) {
    let scale = in_size as f64 / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = PIL_SUPPORT * filterscale;
    // Pillow multiplies by the reciprocal; dividing can differ by an ulp.
    let ss = 1.0 / filterscale;
    let ksize = support.ceil() as usize * 2 + 1;
    let mut bounds = Vec::with_capacity(out_size);
    let mut kernels = vec![0i64; out_size * ksize];
    let mut weights = vec![0f64; ksize];
    for xx in 0..out_size {
        let center = (xx as f64 + 0.5) * scale;
        // C `(int)` truncation, then clamped.
        let xmin = ((center - support + 0.5) as i64).max(0) as usize;
        let xmax = ((center + support + 0.5) as i64).min(in_size as i64) as usize - xmin;
        let mut total = 0.0;
        for (x, w) in weights[..xmax].iter_mut().enumerate() {
            *w = pil_filter((x as f64 + xmin as f64 - center + 0.5) * ss);
            total += *w;
        }
        for (x, w) in weights[..xmax].iter().enumerate() {
            let w = if total != 0.0 { w / total } else { *w };
            let fixed = w * f64::from(1u32 << PRECISION_BITS);
            kernels[xx * ksize + x] = if w < 0.0 {
                (fixed - 0.5) as i64
            } else {
                (fixed + 0.5) as i64
            };
        }
        bounds.push((xmin, xmax));
    }
    (bounds, kernels, ksize)
}

/// PyTorch bicubic resampling (`align_corners=False`, no antialias) of the
/// last axis of `rows` rows of `len` samples. `src_per_dst` is the
/// coordinate scale PyTorch derives from the requested size or factor.
pub fn torch_bicubic_rows(src: &[f64], len: usize, out_len: usize, src_per_dst: f64) -> Vec<f64> {
    let taps: Vec<([usize; 4], [f64; 4])> = (0..out_len)
        .map(|i| {
            let pos = (i as f64 + 0.5) * src_per_dst - 0.5;
            let base = pos.floor();
            let t = pos - base;
            let index = |offset: i64| (base as i64 + offset).clamp(0, len as i64 - 1) as usize;
            (
                [index(-1), index(0), index(1), index(2)],
                [
                    torch_far(t + 1.0),
                    torch_near(t),
                    torch_near(1.0 - t),
                    torch_far(2.0 - t),
                ],
            )
        })
        .collect();
    let rows = src.len() / len;
    let mut out = Vec::with_capacity(rows * out_len);
    for row in src.chunks_exact(len) {
        out.extend(
            taps.iter()
                .map(|(idx, w)| (0..4).map(|k| row[idx[k]] * w[k]).sum::<f64>()),
        );
    }
    out
}

fn torch_near(x: f64) -> f64 {
    ((TORCH_A + 2.0) * x - (TORCH_A + 3.0)) * x * x + 1.0
}

fn torch_far(x: f64) -> f64 {
    ((TORCH_A * x - 5.0 * TORCH_A) * x + 8.0 * TORCH_A) * x - 4.0 * TORCH_A
}

/// PyTorch bicubic resize of `planes` planes of `height`×`width` (planar,
/// row-major): width first, then height, as the reference does. Returns
/// `planes`×`out_h`×`out_w`.
pub fn torch_bicubic_2d(
    src: &[f64],
    (height, width): (usize, usize),
    (out_h, out_w): (usize, usize),
    (src_per_dst_h, src_per_dst_w): (f64, f64),
) -> Vec<f64> {
    let wide = torch_bicubic_rows(src, width, out_w, src_per_dst_w);
    let planes = src.len() / (height * width);
    // Resample columns by transposing each plane.
    let mut out = vec![0f64; planes * out_h * out_w];
    let mut column = vec![0f64; height];
    for p in 0..planes {
        let plane = &wide[p * height * out_w..][..height * out_w];
        for x in 0..out_w {
            for (y, v) in column.iter_mut().enumerate() {
                *v = plane[y * out_w + x];
            }
            for (y, v) in torch_bicubic_rows(&column, height, out_h, src_per_dst_h)
                .into_iter()
                .enumerate()
            {
                out[(p * out_h + y) * out_w + x] = v;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pil_resize_keeps_flat_color() {
        let src = Rgb8 {
            width: 7,
            height: 5,
            data: [10u8, 128, 250].repeat(35),
        };
        let out = pil_resize(&src, 3, 2);
        assert_eq!(out.data, [10u8, 128, 250].repeat(6));
    }

    #[test]
    fn pil_coeffs_sum_to_one() {
        for (src, dst) in [(1999, 1024), (601, 511), (10, 30)] {
            let (bounds, k, ksize) = pil_coeffs(src, dst);
            for (i, &(_, n)) in bounds.iter().enumerate() {
                let sum: i64 = k[i * ksize..][..n].iter().sum();
                assert!(
                    (sum - (1 << PRECISION_BITS)).abs() <= n as i64,
                    "{src}->{dst}"
                );
            }
        }
    }

    #[test]
    fn torch_bicubic_halving_matches_hand_computed_taps() {
        // Output i samples position 2i + 0.5: taps at 2i-1..2i+2, t = 0.5.
        let row: Vec<f64> = (0..8).map(f64::from).collect();
        let out = torch_bicubic_rows(&row, 8, 4, 2.0);
        let w = [
            torch_far(1.5),
            torch_near(0.5),
            torch_near(0.5),
            torch_far(1.5),
        ];
        let expect = |i: i64| -> f64 {
            (0..4)
                .map(|k| (2 * i - 1 + k).clamp(0, 7) as f64 * w[k as usize])
                .sum()
        };
        for (i, v) in out.iter().enumerate() {
            assert!((v - expect(i as i64)).abs() < 1e-12);
        }
        // Interior output on a linear ramp lands on the sample position.
        assert!((out[1] - 2.5).abs() < 1e-12);
    }
}
