//! JPEG artifact removal with DeJPEGNet v2: which pages qualify, and how their
//! pixels pass through the model.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use super::artifacts::DEJPEG_V2;
use super::backend::{Backend, BackendChoice};
use super::runtime::Model;
use super::tiling::{Blender, CHANNELS, Tile, TilePlan, mirror};
use crate::domain::{Decoded, DecodedImage, ImageFrame, Pixels};

const MIN_TILE: usize = 64;
const U8_MAX: f32 = 255.0;
const U16_BITS: u32 = 16;

const GPU_TILE: usize = 512;
/// Faster per output pixel than 512 px on the measured CPU.
const CPU_TILE: usize = 256;
const PROBE_TILE: usize = 64;
const MODEL_INPUT: &str = "input";
const MODEL_OUTPUT: &str = "output";

/// User choices for artifact removal.
#[derive(Debug, Clone, Copy)]
pub struct DejpegOptions {
    /// Restore every input, not only JPEGs.
    pub all: bool,
    pub backend: BackendChoice,
}

/// The loaded model, shared by all conversions.
pub struct Restorer {
    all: bool,
    /// Held for a whole image: the backend parallelizes each run itself, and
    /// this bounds the blend buffers to one image.
    engine: Mutex<Engine>,
}

struct Engine {
    model: Model,
    tile: usize,
}

impl Restorer {
    pub fn load(options: DejpegOptions) -> Result<Self, String> {
        let model = Model::load(&DEJPEG_V2, options.backend, |model| {
            let probe = vec![0.0; CHANNELS * PROBE_TILE * PROBE_TILE];
            model
                .run_rgb(MODEL_INPUT, MODEL_OUTPUT, &probe, PROBE_TILE)
                .map(|_| ())
        })?;
        let tile = if model.backend() == Backend::Cpu {
            CPU_TILE
        } else {
            GPU_TILE
        };
        Ok(Self {
            all: options.all,
            engine: Mutex::new(Engine { model, tile }),
        })
    }

    pub fn all(&self) -> bool {
        self.all
    }

    /// Restore one page in place; ineligible pages pass through with a note.
    pub fn restore(&self, img: &mut DecodedImage, path: &Path) -> Result<(), String> {
        if let Some(reason) = ineligible_reason(img) {
            eprintln!("note: {}: artifacts not removed ({reason})", path.display());
            return Ok(());
        }
        let mut engine = self.engine.lock().unwrap_or_else(PoisonError::into_inner);
        let Engine { model, tile } = &mut *engine;
        restore_with_fallback(&mut img.frame, tile, &mut |input, size| {
            model.run_rgb(MODEL_INPUT, MODEL_OUTPUT, input, size)
        })
    }
}

/// Whether a decoded page goes through the model. JPEGs always do; everything
/// else only with `--dejpeg-all`.
pub fn selects(page: &Decoded, all: bool) -> bool {
    all || matches!(page, Decoded::Jpeg(_))
}

/// Why a page cannot be restored. The model works on SDR samples in 0..=1.
pub fn ineligible_reason(img: &DecodedImage) -> Option<&'static str> {
    if matches!(img.frame.pixels, Pixels::F32(_)) {
        Some("floating-point samples")
    } else if img.hdr.intensity_target.is_some() {
        Some("HDR")
    } else if img.frame.alpha_premultiplied {
        Some("premultiplied alpha")
    } else {
        None
    }
}

/// Restore `frame`, halving the tile size whenever inference fails; the size
/// that worked stays in `tile` for later images. Pixels are only written once
/// the whole image has succeeded.
pub fn restore_with_fallback(
    frame: &mut ImageFrame,
    tile: &mut usize,
    infer: &mut impl FnMut(&[f32], usize) -> Result<Vec<f32>, String>,
) -> Result<(), String> {
    loop {
        match restore_frame(frame, *tile, infer) {
            Ok(()) => return Ok(()),
            Err(e) if *tile <= MIN_TILE => return Err(e),
            Err(e) => {
                *tile /= 2;
                eprintln!(
                    "note: restore failed ({e}); retrying with {} px tiles",
                    *tile
                );
            }
        }
    }
}

fn restore_frame(
    frame: &mut ImageFrame,
    size: usize,
    infer: &mut impl FnMut(&[f32], usize) -> Result<Vec<f32>, String>,
) -> Result<(), String> {
    let (width, height) = (frame.width as usize, frame.height as usize);
    let plan = TilePlan::new(width, height, size);
    let mut blender = Blender::new(width, height);
    let mut input = vec![0.0; CHANNELS * size * size];
    for &tile in &plan.tiles {
        fill_tile(frame, &plan, tile, &mut input)?;
        let output = infer(&input, size)?;
        if output.len() != input.len() || !output.iter().all(|v| v.is_finite()) {
            return Err("model returned invalid output".into());
        }
        blender.add(&plan, tile, &output);
    }
    let mut rgb = blender.finish();
    if is_neutral(frame) {
        // The model could tint grayscale content.
        for pixel in rgb.chunks_exact_mut(CHANNELS) {
            let mean = pixel.iter().sum::<f32>() / CHANNELS as f32;
            pixel.fill(mean);
        }
    }
    store(frame, &rgb)
}

fn samples_per_pixel(frame: &ImageFrame) -> usize {
    CHANNELS + usize::from(frame.has_alpha)
}

/// Shift and top value of N-bit samples left-aligned in 16 bits.
fn u16_layout(bit_depth: u8) -> (u32, f32) {
    let depth = u32::from(bit_depth).clamp(1, U16_BITS);
    (U16_BITS - depth, ((1u32 << depth) - 1) as f32)
}

fn fill_tile(
    frame: &ImageFrame,
    plan: &TilePlan,
    tile: Tile,
    out: &mut [f32],
) -> Result<(), String> {
    match &frame.pixels {
        Pixels::U8(v) => fill_from(v, frame, plan, tile, out, |s| f32::from(s) / U8_MAX),
        Pixels::U16(v) => {
            let (shift, top) = u16_layout(frame.bit_depth);
            fill_from(v, frame, plan, tile, out, |s| f32::from(s >> shift) / top);
        }
        Pixels::F32(_) => return Err("internal: float frame reached the model".into()),
    }
    Ok(())
}

fn fill_from<T: Copy>(
    samples: &[T],
    frame: &ImageFrame,
    plan: &TilePlan,
    tile: Tile,
    out: &mut [f32],
    to_unit: impl Fn(T) -> f32,
) {
    let stride = samples_per_pixel(frame);
    let size = plan.size;
    for dy in 0..size {
        let y = mirror(tile.y0 + dy, plan.height);
        for dx in 0..size {
            let pixel = (y * plan.width + mirror(tile.x0 + dx, plan.width)) * stride;
            for c in 0..CHANNELS {
                out[(c * size + dy) * size + dx] = to_unit(samples[pixel + c]);
            }
        }
    }
}

fn is_neutral(frame: &ImageFrame) -> bool {
    fn neutral<T: Copy + PartialEq>(samples: &[T], stride: usize) -> bool {
        samples
            .chunks_exact(stride)
            .all(|p| p[0] == p[1] && p[1] == p[2])
    }
    let stride = samples_per_pixel(frame);
    match &frame.pixels {
        Pixels::U8(v) => neutral(v, stride),
        Pixels::U16(v) => neutral(v, stride),
        Pixels::F32(v) => neutral(v, stride),
    }
}

/// Write interleaved RGB back, leaving any alpha samples alone.
fn store(frame: &mut ImageFrame, rgb: &[f32]) -> Result<(), String> {
    let stride = samples_per_pixel(frame);
    let bit_depth = frame.bit_depth;
    match &mut frame.pixels {
        Pixels::U8(v) => store_into(v, stride, rgb, |s| (s * U8_MAX).round() as u8),
        Pixels::U16(v) => {
            let (shift, top) = u16_layout(bit_depth);
            store_into(v, stride, rgb, |s| ((s * top).round() as u16) << shift);
        }
        Pixels::F32(_) => return Err("internal: float frame reached the model".into()),
    }
    Ok(())
}

fn store_into<T>(samples: &mut [T], stride: usize, rgb: &[f32], from_unit: impl Fn(f32) -> T) {
    for (pixel, restored) in samples
        .chunks_exact_mut(stride)
        .zip(rgb.chunks_exact(CHANNELS))
    {
        for (sample, &value) in pixel.iter_mut().zip(restored) {
            *sample = from_unit(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ColorDesc, Hdr, MetaBoxes};
    use std::sync::Arc;

    fn frame(pixels: Pixels, bit_depth: u8, has_alpha: bool, w: u32, h: u32) -> ImageFrame {
        ImageFrame {
            width: w,
            height: h,
            bit_depth,
            has_alpha,
            alpha_premultiplied: false,
            pixels,
        }
    }

    fn image(frame: ImageFrame) -> DecodedImage {
        DecodedImage::new(frame, ColorDesc::new(None, None), MetaBoxes::new(None, None))
    }

    /// Returns the input unchanged.
    fn identity(input: &[f32], _size: usize) -> Result<Vec<f32>, String> {
        Ok(input.to_vec())
    }

    #[test]
    fn jpeg_is_always_selected_other_pages_only_with_all() {
        let jpeg = Decoded::Jpeg(Arc::new(Vec::new()));
        let page = Decoded::Image(Box::new(image(frame(Pixels::U8(vec![0; 3]), 8, false, 1, 1))));
        assert!(selects(&jpeg, false));
        assert!(selects(&jpeg, true));
        assert!(!selects(&page, false));
        assert!(selects(&page, true));
    }

    #[test]
    fn float_hdr_and_premultiplied_are_ineligible() {
        let plain = || frame(Pixels::U8(vec![0; 3]), 8, false, 1, 1);
        assert_eq!(ineligible_reason(&image(plain())), None);
        let float = image(frame(Pixels::F32(vec![0.0; 3]), 32, false, 1, 1));
        assert!(ineligible_reason(&float).is_some());
        let hdr = image(plain()).hdr(Hdr::for_transfer(16));
        assert!(ineligible_reason(&hdr).is_some());
        let mut premultiplied = frame(Pixels::U8(vec![0; 4]), 8, true, 1, 1);
        premultiplied.alpha_premultiplied = true;
        assert!(ineligible_reason(&image(premultiplied)).is_some());
    }

    #[test]
    fn identity_keeps_u8_pixels_and_alpha() {
        let pixels: Vec<u8> = (0..4 * 80 * 70).map(|i| (i % 255) as u8).collect();
        let mut f = frame(Pixels::U8(pixels.clone()), 8, true, 80, 70);
        restore_frame(&mut f, 64, &mut identity).unwrap();
        let Pixels::U8(out) = &f.pixels else {
            panic!("storage type changed");
        };
        assert_eq!(*out, pixels);
    }

    #[test]
    fn identity_keeps_left_aligned_u16_at_every_depth() {
        for depth in [10u8, 12, 16] {
            let shift = 16 - u32::from(depth);
            let max = (1u32 << depth) - 1;
            let pixels: Vec<u16> = (0..3 * 80 * 70u32)
                .map(|i| ((i * 37 % (max + 1)) << shift) as u16)
                .collect();
            let mut f = frame(Pixels::U16(pixels.clone()), depth, false, 80, 70);
            restore_frame(&mut f, 64, &mut identity).unwrap();
            let Pixels::U16(out) = &f.pixels else {
                panic!("storage type changed");
            };
            assert_eq!(*out, pixels, "depth {depth}");
        }
    }

    #[test]
    fn neutral_input_stays_neutral_when_the_model_adds_color() {
        let mut f = frame(Pixels::U8(vec![128; 3 * 20 * 20]), 8, false, 20, 20);
        let mut tint = |input: &[f32], size: usize| -> Result<Vec<f32>, String> {
            let mut out = input.to_vec();
            out[..size * size].iter_mut().for_each(|v| *v = 0.9);
            Ok(out)
        };
        restore_frame(&mut f, 64, &mut tint).unwrap();
        let Pixels::U8(out) = &f.pixels else {
            panic!("storage type changed");
        };
        assert!(out.chunks_exact(3).all(|p| p[0] == p[1] && p[1] == p[2]));
    }

    #[test]
    fn colored_input_keeps_model_color() {
        let mut pixels = vec![128u8; 3 * 20 * 20];
        pixels[0] = 10;
        let mut f = frame(Pixels::U8(pixels), 8, false, 20, 20);
        let mut tint = |input: &[f32], size: usize| -> Result<Vec<f32>, String> {
            let mut out = input.to_vec();
            out[..size * size].iter_mut().for_each(|v| *v = 0.9);
            Ok(out)
        };
        restore_frame(&mut f, 64, &mut tint).unwrap();
        let Pixels::U8(out) = &f.pixels else {
            panic!("storage type changed");
        };
        assert!(out.chunks_exact(3).any(|p| p[0] != p[1]));
    }

    #[test]
    fn non_finite_or_short_output_is_an_error() {
        let mut f = frame(Pixels::U8(vec![1; 3 * 8 * 8]), 8, false, 8, 8);
        let mut nan = |input: &[f32], _: usize| Ok(vec![f32::NAN; input.len()]);
        assert!(restore_frame(&mut f, 64, &mut nan).is_err());
        let mut short = |_: &[f32], _: usize| Ok(vec![0.0; 3]);
        assert!(restore_frame(&mut f, 64, &mut short).is_err());
    }

    #[test]
    fn failure_halves_the_tile_and_keeps_the_smaller_size() {
        let mut f = frame(Pixels::U8(vec![9; 3 * 300 * 300]), 8, false, 300, 300);
        let mut tile = 512;
        let mut only_small = |input: &[f32], size: usize| {
            if size > 128 {
                Err("out of memory".to_string())
            } else {
                Ok(input.to_vec())
            }
        };
        restore_with_fallback(&mut f, &mut tile, &mut only_small).unwrap();
        assert_eq!(tile, 128);
    }

    #[test]
    fn failure_at_the_floor_fails_and_leaves_pixels_untouched() {
        let mut f = frame(Pixels::U8(vec![9; 3 * 20 * 20]), 8, false, 20, 20);
        let mut tile = 128;
        let mut seen = Vec::new();
        let mut broken = |_: &[f32], size: usize| {
            seen.push(size);
            Err::<Vec<f32>, _>("broken".to_string())
        };
        assert!(restore_with_fallback(&mut f, &mut tile, &mut broken).is_err());
        assert_eq!(seen, [128, 64]);
        assert!(matches!(&f.pixels, Pixels::U8(v) if v.iter().all(|&p| p == 9)));
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        const PEAK: f64 = 255.0;
        let mse = a
            .iter()
            .zip(b)
            .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
            .sum::<f64>()
            / a.len() as f64;
        10.0 * (PEAK * PEAK / mse).log10()
    }

    fn decode_rgb(jpeg: &[u8]) -> DecodedImage {
        match crate::format::jpeg::decode(jpeg).unwrap() {
            Decoded::Image(img) => *img,
            Decoded::Jpeg(_) => panic!("pixel decode expected"),
        }
    }

    #[cfg(feature = "ai")]
    #[test]
    #[ignore = "downloads ONNX Runtime and the 15 MB model"]
    fn restoring_a_compressed_image_improves_psnr() {
        const LOW_QUALITY: u8 = 20;
        let original = decode_rgb(include_bytes!("../../.github/smoke/plasma.jpg"));
        let (width, height) = (original.frame.width, original.frame.height);
        let Pixels::U8(reference) = &original.frame.pixels else {
            panic!("8-bit JPEG expected");
        };
        let mut jpeg = Vec::new();
        jpeg_encoder::Encoder::new(&mut jpeg, LOW_QUALITY)
            .encode(reference, width as u16, height as u16, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        let mut degraded = decode_rgb(&jpeg);
        let before = match &degraded.frame.pixels {
            Pixels::U8(v) => psnr(reference, v),
            _ => panic!("8-bit JPEG expected"),
        };
        let restorer = Restorer::load(DejpegOptions {
            all: false,
            backend: BackendChoice::Cpu,
        })
        .unwrap();
        restorer.restore(&mut degraded, Path::new("test")).unwrap();
        let after = match &degraded.frame.pixels {
            Pixels::U8(v) => psnr(reference, v),
            _ => panic!("8-bit JPEG expected"),
        };
        assert!(after > before, "{after} dB is not above {before} dB");
    }
}
