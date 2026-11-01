//! Rust port of the Charm tokenizer (frequency-based patch selection, two
//! scales, factor 0.5, no padding or dropping), following the NumPy port at
//! https://github.com/Miosp/charm-onnx. Reference quirks (pad-or-crop rule,
//! position order of high-resolution tokens) are kept on purpose: the
//! checkpoints were trained with them.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

use super::resample::{Rgb8, pil_resize, torch_bicubic_2d};

pub const PATCH: usize = 14;
/// A high-resolution region is 2×2 tokens; a low-resolution one, 1 token.
const REGION: usize = 2 * PATCH;
const HIGH_RES_REGIONS: usize = 64;
const POS_INTERPOLATE_OFFSET: f64 = 0.1;
const LOW_RES: i64 = 0;
const HIGH_RES: i64 = 1;
const RGB: usize = 3;
const PATCH_LEN: usize = RGB * PATCH * PATCH;

/// Model input for one image; token 0 is the CLS placeholder.
pub struct Tokens {
    /// `[n, 3, 14, 14]`.
    pub patches: Vec<f32>,
    /// `[n, dim]`.
    pub pos_embed: Vec<f32>,
    pub scale_ids: Vec<i64>,
}

/// The backbone's positional table: CLS row plus a square grid.
pub struct PosTable {
    dim: usize,
    side: usize,
    cls: Vec<f32>,
    /// `(dim, side, side)` planes.
    grid: Vec<f64>,
}

impl PosTable {
    /// `values`: `[1 + side², dim]` row-major, as stored in `pos_embed.npy`.
    pub fn new(values: &[f32], dim: usize) -> Result<Self, String> {
        let rows = values.len() / dim.max(1);
        let side = ((rows.saturating_sub(1)) as f64).sqrt().round() as usize;
        if dim == 0 || rows * dim != values.len() || 1 + side * side != rows {
            return Err("position table is not CLS + square grid".into());
        }
        let mut grid = vec![0f64; dim * side * side];
        for (cell, row) in values[dim..].chunks_exact(dim).enumerate() {
            for (d, &v) in row.iter().enumerate() {
                grid[d * side * side + cell] = f64::from(v);
            }
        }
        Ok(Self {
            dim,
            side,
            cls: values[..dim].to_vec(),
            grid,
        })
    }

    /// Embeddings interpolated to a `rows`×`cols` token grid, raster order.
    fn grid(&self, rows: usize, cols: usize) -> Vec<f32> {
        let side = self.side as f64;
        let planes = torch_bicubic_2d(
            &self.grid,
            (self.side, self.side),
            (rows, cols),
            (
                side / (rows as f64 + POS_INTERPOLATE_OFFSET),
                side / (cols as f64 + POS_INTERPOLATE_OFFSET),
            ),
        );
        let cells = rows * cols;
        let mut out = vec![0f32; cells * self.dim];
        for (d, plane) in planes.chunks_exact(cells).enumerate() {
            for (cell, &v) in plane.iter().enumerate() {
                out[cell * self.dim + d] = v as f32;
            }
        }
        out
    }
}

/// Fit the reference's input size: images whose longer side exceeds
/// `max_side` are downscaled with Pillow's bicubic filter.
pub fn fit(image: Rgb8, max_side: usize) -> Rgb8 {
    let (w, h) = (image.width, image.height);
    if w.max(h) <= max_side {
        return image;
    }
    let ratio = (max_side as f64 / w as f64).min(max_side as f64 / h as f64);
    // Python `int(w * ratio)` truncates.
    let (nw, nh) = ((w as f64 * ratio) as usize, (h as f64 * ratio) as usize);
    pil_resize(&image, nw.max(1), nh.max(1))
}

/// Planar `[3, h, w]` image in 0..=1.
struct Planar {
    width: usize,
    height: usize,
    data: Vec<f32>,
}

impl Planar {
    fn from_rgb8(image: &Rgb8) -> Self {
        let plane = image.width * image.height;
        let mut data = vec![0f32; RGB * plane];
        for (i, px) in image.data.as_chunks::<RGB>().0.iter().enumerate() {
            for c in 0..RGB {
                data[c * plane + i] = f32::from(px[c]) / 255.0;
            }
        }
        Self {
            width: image.width,
            height: image.height,
            data,
        }
    }

    /// Zero padding at the bottom/right or cropping, to `width`×`height`.
    fn resized_canvas(&self, width: usize, height: usize) -> Self {
        let mut data = vec![0f32; RGB * width * height];
        let (w, h) = (self.width.min(width), self.height.min(height));
        for c in 0..RGB {
            for y in 0..h {
                let src = &self.data[(c * self.height + y) * self.width..][..w];
                data[(c * height + y) * width..][..w].copy_from_slice(src);
            }
        }
        Self {
            width,
            height,
            data,
        }
    }

    /// `size`×`size` blocks in raster order, each `[3, size, size]`.
    fn blocks(&self, size: usize) -> Vec<Vec<f32>> {
        let (rows, cols) = (self.height / size, self.width / size);
        let mut out = Vec::with_capacity(rows * cols);
        for r in 0..rows {
            for q in 0..cols {
                let mut block = Vec::with_capacity(RGB * size * size);
                for c in 0..RGB {
                    for y in 0..size {
                        let start = (c * self.height + r * size + y) * self.width + q * size;
                        block.extend_from_slice(&self.data[start..start + size]);
                    }
                }
                out.push(block);
            }
        }
        out
    }
}

/// Tokens for an image already fitted with [`fit`].
pub fn tokenize(image: &Rgb8, pos: &PosTable, max_tokens: usize) -> Result<Tokens, String> {
    let planar = Planar::from_rgb8(image);
    let (body_patches, body_pos, body_ids) =
        if image.height.div_ceil(PATCH) * image.width.div_ceil(PATCH) <= max_tokens {
            single_scale(&planar, pos)
        } else {
            multi_scale(&planar, pos)?
        };
    let mut tokens = Tokens {
        patches: vec![0f32; PATCH_LEN],
        pos_embed: pos.cls.clone(),
        scale_ids: vec![LOW_RES],
    };
    tokens.patches.extend(body_patches);
    tokens.pos_embed.extend(body_pos);
    tokens.scale_ids.extend(body_ids);
    Ok(tokens)
}

type Body = (Vec<f32>, Vec<f32>, Vec<i64>);

fn single_scale(image: &Planar, pos: &PosTable) -> Body {
    let (rows, cols) = (image.height.div_ceil(PATCH), image.width.div_ceil(PATCH));
    let padded = image.resized_canvas(cols * PATCH, rows * PATCH);
    let patches = padded.blocks(PATCH).concat();
    (patches, pos.grid(rows, cols), vec![LOW_RES; rows * cols])
}

fn multi_scale(image: &Planar, pos: &PosTable) -> Result<Body, String> {
    let (rows, cols) = pad_or_crop_grid(image.height, image.width);
    let canvas = image.resized_canvas(cols * REGION, rows * REGION);
    let regions = canvas.blocks(REGION);
    if regions.len() < HIGH_RES_REGIONS {
        return Err(format!(
            "{} regions, fewer than {HIGH_RES_REGIONS}",
            regions.len()
        ));
    }

    let sharpness = sharpness(&regions);
    let mut ranked: Vec<usize> = (0..regions.len()).collect();
    // Stable ascending sort, as NumPy's `kind="stable"`; -inf sorts first.
    ranked.sort_by(|&a, &b| sharpness[a].total_cmp(&sharpness[b]));
    let mut is_high = vec![false; regions.len()];
    for &i in &ranked[ranked.len() - HIGH_RES_REGIONS..] {
        is_high[i] = true;
    }

    let low: Vec<usize> = (0..regions.len()).filter(|&i| !is_high[i]).collect();
    let mut patches = Vec::with_capacity((low.len() + 4 * HIGH_RES_REGIONS) * PATCH_LEN);
    for &i in &low {
        let region: Vec<f64> = regions[i].iter().map(|&v| f64::from(v)).collect();
        let halved = torch_bicubic_2d(&region, (REGION, REGION), (PATCH, PATCH), (2.0, 2.0));
        patches.extend(halved.into_iter().map(|v| v as f32));
    }
    // Each high-res region splits into its four patches, row by row.
    for (region, _) in regions.iter().zip(&is_high).filter(|(_, high)| **high) {
        for sub in 0..4 {
            let (oy, ox) = ((sub / 2) * PATCH, (sub % 2) * PATCH);
            for c in 0..RGB {
                for y in 0..PATCH {
                    let start = (c * REGION + oy + y) * REGION + ox;
                    patches.extend_from_slice(&region[start..start + PATCH]);
                }
            }
        }
    }

    // High-res positions follow raster order of the fine grid, not token
    // order (region by region); the reference does the same.
    let dim = pos.dim;
    let coarse = pos.grid(rows, cols);
    let fine = pos.grid(2 * rows, 2 * cols);
    let mut pos_embed = Vec::with_capacity((low.len() + 4 * HIGH_RES_REGIONS) * dim);
    for &i in &low {
        pos_embed.extend_from_slice(&coarse[i * dim..(i + 1) * dim]);
    }
    for fy in 0..2 * rows {
        for fx in 0..2 * cols {
            if is_high[(fy / 2) * cols + fx / 2] {
                let cell = fy * 2 * cols + fx;
                pos_embed.extend_from_slice(&fine[cell * dim..(cell + 1) * dim]);
            }
        }
    }
    let mut scale_ids = vec![LOW_RES; low.len()];
    scale_ids.resize(low.len() + 4 * HIGH_RES_REGIONS, HIGH_RES);
    Ok((patches, pos_embed, scale_ids))
}

/// Region grid after the reference's pad-or-crop rule: whichever of
/// padding up or cropping down to a multiple of the region size touches
/// the smaller area (ties pad).
fn pad_or_crop_grid(height: usize, width: usize) -> (usize, usize) {
    let crop_area = (height % REGION).max(1) * (width % REGION).max(1);
    let pad = |n: usize| match (REGION - n % REGION) % REGION {
        0 => 1,
        p => p,
    };
    if pad(height) * pad(width) > crop_area {
        (height / REGION, width / REGION)
    } else {
        (height.div_ceil(REGION), width.div_ceil(REGION))
    }
}

/// Mean log-magnitude spectrum per region (the reference's blur metric).
fn sharpness(regions: &[Vec<f32>]) -> Vec<f64> {
    let fft = FftPlanner::<f64>::new().plan_fft_forward(REGION);
    let mut buf = vec![Complex::new(0f64, 0f64); REGION * REGION];
    let mut column = vec![Complex::new(0f64, 0f64); REGION];
    regions
        .iter()
        .map(|region| {
            let mut total = 0f64;
            for channel in region.as_chunks::<{ REGION * REGION }>().0 {
                for (b, &v) in buf.iter_mut().zip(channel) {
                    *b = Complex::new(f64::from(v), 0.0);
                }
                fft.process(&mut buf); // every row
                for x in 0..REGION {
                    for (y, c) in column.iter_mut().enumerate() {
                        *c = buf[y * REGION + x];
                    }
                    fft.process(&mut column);
                    for (y, c) in column.iter().enumerate() {
                        buf[y * REGION + x] = *c;
                    }
                }
                total += buf.iter().map(|c| 20.0 * c.norm().log10()).sum::<f64>();
            }
            total / (RGB * REGION * REGION) as f64
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("testdata/charm_golden.txt");
    /// Tokenizer-only tolerance on sums of 588 (patch) or 384 (position)
    /// values; one swapped high-res region would be far outside it.
    const SUM_TOLERANCE: f64 = 2e-3;

    /// Same image as `pattern` in `testdata/make_golden.py`.
    pub(crate) fn pattern(width: usize, height: usize) -> Rgb8 {
        let mut data = Vec::with_capacity(width * height * RGB);
        for y in 0..height as u64 {
            for x in 0..width as u64 {
                for c in 0..RGB as u64 {
                    let mixed =
                        ((x * 73_856_093) ^ (y * 19_349_663) ^ (c * 83_492_791)) & 0xFFFF_FFFF;
                    let noise = (mixed.wrapping_mul(2_654_435_761) & 0xFFFF_FFFF) >> 24;
                    let base =
                        (x * 255 / (width as u64 - 1) + y * 255 / (height as u64 - 1) + c * 85)
                            % 256;
                    let amp = (x / 53 + y / 71) % 7;
                    data.push(((base * (8 - amp) + noise * amp) / 8) as u8);
                }
            }
        }
        Rgb8 {
            width,
            height,
            data,
        }
    }

    pub(crate) struct Case {
        pub width: usize,
        pub height: usize,
        pub scale_ids: Vec<i64>,
        pub patch_sums: Vec<f64>,
        pub pos_sums: Vec<f64>,
        pub scores: Vec<(String, f32)>,
    }

    pub(crate) fn golden_cases() -> Vec<Case> {
        let mut cases: Vec<Case> = Vec::new();
        for line in GOLDEN.lines() {
            let mut words = line.split_whitespace();
            let key = words.next().unwrap_or_default();
            let nums = |w: std::str::SplitWhitespace| {
                w.map(|v| v.parse::<f64>().unwrap()).collect::<Vec<_>>()
            };
            match key {
                "case" => {
                    let v = nums(words);
                    cases.push(Case {
                        width: v[0] as usize,
                        height: v[1] as usize,
                        scale_ids: Vec::new(),
                        patch_sums: Vec::new(),
                        pos_sums: Vec::new(),
                        scores: Vec::new(),
                    });
                }
                "scale_ids" => {
                    let ids = words
                        .next()
                        .unwrap()
                        .bytes()
                        .map(|b| i64::from(b - b'0'))
                        .collect();
                    cases.last_mut().unwrap().scale_ids = ids;
                }
                "patch_sums" => cases.last_mut().unwrap().patch_sums = nums(words),
                "pos_sums" => cases.last_mut().unwrap().pos_sums = nums(words),
                "score" => {
                    let model = words.next().unwrap().to_string();
                    let value = words.next().unwrap().parse().unwrap();
                    cases.last_mut().unwrap().scores.push((model, value));
                }
                _ => {}
            }
        }
        cases
    }

    /// Same table as `synthetic_pos_table` in `testdata/make_golden.py`
    /// (DINOv2-small shape).
    fn synthetic_pos_table() -> PosTable {
        const SIDE: usize = 37;
        const DIM: usize = 384;
        let values: Vec<f32> = (0..(1 + SIDE * SIDE) * DIM)
            .map(|i| ((i / DIM) as f64 * 0.013 + (i % DIM) as f64 * 0.071).sin() as f32)
            .collect();
        PosTable::new(&values, DIM).unwrap()
    }

    #[test]
    fn matches_reference_tokens() {
        let pos = synthetic_pos_table();
        for case in golden_cases() {
            let image = fit(pattern(case.width, case.height), 1024);
            let tokens = tokenize(&image, &pos, 1025).unwrap();
            let name = format!("{}x{}", case.width, case.height);
            assert_eq!(tokens.scale_ids, case.scale_ids, "{name} scale ids");
            for (i, (patch, want)) in tokens
                .patches
                .as_chunks::<PATCH_LEN>()
                .0
                .iter()
                .zip(&case.patch_sums)
                .enumerate()
            {
                let got: f64 = patch.iter().map(|&v| f64::from(v)).sum();
                assert!(
                    (got - want).abs() < SUM_TOLERANCE,
                    "{name} patch {i}: {got} vs {want}"
                );
            }
            for (i, (row, want)) in tokens
                .pos_embed
                .chunks_exact(pos.dim)
                .zip(&case.pos_sums)
                .enumerate()
            {
                let got: f64 = row.iter().map(|&v| f64::from(v)).sum();
                assert!(
                    (got - want).abs() < SUM_TOLERANCE,
                    "{name} pos {i}: {got} vs {want}"
                );
            }
        }
    }

    #[test]
    fn fit_truncates_like_python() {
        let image = fit(pattern(1999, 1333), 1024);
        // 1999 * (1024 / 1999) lands just below 1024; Python truncates too.
        assert_eq!((image.width, image.height), (1023, 682));
        let image = fit(pattern(601, 1203), 1024);
        assert_eq!((image.width, image.height), (511, 1024));
    }

    #[test]
    fn pad_or_crop_follows_area_rule() {
        // 1000 % 28 = 20: padding 8 rows/cols touches less than cropping 20.
        assert_eq!(pad_or_crop_grid(1000, 1000), (36, 36));
        // 30 % 28 = 2: cropping 2 beats padding 26.
        assert_eq!(pad_or_crop_grid(30 * 28 + 2, 28 * 10), (30, 10));
    }
}
