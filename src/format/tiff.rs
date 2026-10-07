//! TIFF decode: multipage → one Decoded per page; ICC tag; 8/16-bit;
//! float → F32; CMYK → RGB.

use crate::domain::{
    ColorDesc, Decoded, DecodedImage, ImageFrame, MetaBoxes, Orientation, Pixels,
};

/// Per-page pixel budget (268M px ≈ 8k×33k; RGBA16 ≈ 2 GiB).
const MAX_PAGE_PIXELS: u64 = 1 << 28;
/// Aggregate budget across pages — bounds the multi-page bomb.
const MAX_TOTAL_PIXELS: u64 = 1 << 30;

pub fn decode(data: &std::sync::Arc<Vec<u8>>) -> Result<Vec<Decoded>, String> {
    // Crate defaults (256 MiB) would reject pages our own budget allows:
    // size limits for the worst case, RGBA16 (8 B/px).
    let mut limits = tiff::decoder::Limits::default();
    limits.decoding_buffer_size = (MAX_PAGE_PIXELS * 8) as usize;
    limits.intermediate_buffer_size = 512 * 1024 * 1024;
    let mut dec = tiff::decoder::Decoder::new(std::io::Cursor::new(data.as_slice()))
        .map_err(|e| format!("TIFF open: {e}"))?
        .with_limits(limits);
    let mut total_pixels = 0u64;
    let mut pages = Vec::new();

    loop {
        let (w, h) = dec.dimensions().map_err(|e| format!("TIFF dims: {e}"))?;
        let ct = dec.colortype().map_err(|e| format!("TIFF colortype: {e}"))?;
        let pixels = u64::from(w) * u64::from(h);
        if pixels > MAX_PAGE_PIXELS {
            return Err(format!("TIFF page too large ({w}x{h})"));
        }
        total_pixels += pixels;
        if total_pixels > MAX_TOTAL_PIXELS {
            return Err("TIFF total decoded size too large".into());
        }
        let icc = extract_icc(&mut dec);
        let orientation = dec
            .find_tag(tiff::tags::Tag::Orientation)
            .ok()
            .flatten()
            .and_then(|v| v.into_u32().ok())
            .and_then(|v| u8::try_from(v).ok())
            .map(Orientation::new)
            .unwrap_or_default();

        let img = dec.read_image().map_err(|e| format!("TIFF decode: {e}"))?;
        let frame = frame_from(w, h, ct, img)?;

        // TIFF is itself an EXIF container; pass the whole file (shared with
        // the decoder input, no copy) so the IFD metadata survives in the
        // JXL Exif box.
        let meta = MetaBoxes {
            exif: Some(std::sync::Arc::clone(data)),
            ..MetaBoxes::default()
        };
        pages.push(Decoded::Image(Box::new(
            DecodedImage::new(frame, ColorDesc::new(None, icc), meta).orientation(orientation),
        )));

        if !dec.more_images() {
            break;
        }
        // Count check before reading the next page: a >64-page bomb must
        // not pay for page 65's decode.
        if pages.len() >= 64 {
            return Err("TIFF with more than 64 pages not supported".into());
        }
        dec.next_image()
            .map_err(|e| format!("TIFF next page: {e}"))?;
    }
    Ok(pages)
}

fn frame_from(
    w: u32,
    h: u32,
    ct: tiff::ColorType,
    img: tiff::decoder::DecodingResult,
) -> Result<ImageFrame, String> {
    use tiff::ColorType;
    use tiff::decoder::DecodingResult;
    // Sub-8-bit samples arrive bit-packed (MSB-first, per row): expand to
    // full-range 8-bit before the sample-layout conversions below.
    let bits = ct.bit_depth();
    let img = match ((1..8).contains(&bits), img) {
        (true, DecodingResult::U8(v)) => {
            DecodingResult::U8(unpack_bits(&v, w, h, samples_per_pixel(ct), bits)?)
        }
        (_, img) => img,
    };
    let frame = |bit_depth, has_alpha, pixels| ImageFrame {
        width: w,
        height: h,
        bit_depth,
        has_alpha,
        pixels,
    };
    Ok(match (ct, img) {
        (ColorType::RGB(_), DecodingResult::U8(v)) => frame(8, false, Pixels::U8(v)),
        (ColorType::RGBA(_), DecodingResult::U8(v)) => frame(8, true, Pixels::U8(v)),
        (ColorType::Gray(_), DecodingResult::U8(v)) => {
            frame(8, false, Pixels::U8(replicate(&v)))
        }
        (ColorType::GrayA(_), DecodingResult::U8(v)) => frame(8, true, Pixels::U8(interleave(&v))),
        (ColorType::RGB(_), DecodingResult::U16(v)) => frame(16, false, Pixels::U16(v)),
        (ColorType::RGBA(_), DecodingResult::U16(v)) => frame(16, true, Pixels::U16(v)),
        (ColorType::Gray(_), DecodingResult::U16(v)) => {
            frame(16, false, Pixels::U16(replicate(&v)))
        }
        (ColorType::GrayA(_), DecodingResult::U16(v)) => {
            frame(16, true, Pixels::U16(interleave(&v)))
        }
        (ColorType::CMYK(_), DecodingResult::U8(v)) => {
            frame(8, false, Pixels::U8(cmyk_to_rgb8(&v)))
        }
        (ColorType::CMYK(_), DecodingResult::U16(v)) => {
            frame(16, false, Pixels::U16(cmyk_to_rgb16(&v)))
        }
        // Float samples: encode as f32 (JXL float32); exact photometric
        // interpretation requires an ICC profile on the file.
        (ColorType::RGB(_) | ColorType::RGBA(_), DecodingResult::F32(v)) => {
            frame(32, matches!(ct, ColorType::RGBA(_)), Pixels::F32(v))
        }
        (ColorType::Gray(_), DecodingResult::F32(v)) => {
            frame(32, false, Pixels::F32(replicate(&v)))
        }
        (ColorType::GrayA(_), DecodingResult::F32(v)) => {
            frame(32, true, Pixels::F32(interleave(&v)))
        }
        (ColorType::CMYK(_), DecodingResult::F32(_)) => {
            return Err("float CMYK TIFF not supported".into())
        }
        (ct, r) => {
            return Err(format!(
                "unsupported TIFF combination {ct:?} / {} samples",
                sample_kind(&r)
            ))
        }
    })
}

fn samples_per_pixel(ct: tiff::ColorType) -> usize {
    use tiff::ColorType::*;
    match ct {
        Gray(_) => 1,
        GrayA(_) => 2,
        RGB(_) => 3,
        RGBA(_) | CMYK(_) | CMYKA(_) => 4,
        _ => 1,
    }
}

/// Expand bit-packed sub-8-bit TIFF samples to full-range u8
/// (v -> v*255/(2^bits-1)); rows may be byte-padded to `stride`.
fn unpack_bits(
    buf: &[u8],
    w: u32,
    h: u32,
    samples: usize,
    bits: u8,
) -> Result<Vec<u8>, String> {
    let (w, h, bits) = (w as usize, h as usize, bits as usize);
    let packed_row = (w * samples * bits).div_ceil(8);
    let stride = buf
        .len()
        .checked_div(h)
        .filter(|s| *s >= packed_row)
        .ok_or("TIFF sub-8-bit buffer size mismatch")?;
    let max = (1u32 << bits) - 1;
    let mut out = Vec::with_capacity(w * samples * h);
    for row in buf.chunks_exact(stride).take(h) {
        let (mut acc, mut acc_bits, mut bi) = (0u32, 0usize, 0usize);
        for _ in 0..w * samples {
            while acc_bits < bits {
                acc = (acc << 8) | u32::from(row[bi]);
                bi += 1;
                acc_bits += 8;
            }
            let v = (acc >> (acc_bits - bits)) & max;
            acc_bits -= bits;
            out.push((v * 255 / max) as u8);
        }
    }
    Ok(out)
}

fn sample_kind(r: &tiff::decoder::DecodingResult) -> &'static str {
    use tiff::decoder::DecodingResult;
    match r {
        DecodingResult::U8(_) => "u8",
        DecodingResult::U16(_) => "u16",
        DecodingResult::U32(_) => "u32",
        DecodingResult::U64(_) => "u64",
        DecodingResult::F32(_) => "f32",
        DecodingResult::F64(_) => "f64",
        _ => "other",
    }
}

fn extract_icc<R: std::io::Read + std::io::Seek>(
    dec: &mut tiff::decoder::Decoder<R>,
) -> Option<Vec<u8>> {
    match dec.find_tag(tiff::tags::Tag::IccProfile) {
        Ok(Some(v)) => v.into_u8_vec().ok(),
        _ => None,
    }
}

/// Gray → RGB.
fn replicate<T: Copy>(v: &[T]) -> Vec<T> {
    let mut out = Vec::with_capacity(v.len() * 3);
    for &s in v {
        out.extend_from_slice([s, s, s].as_slice());
    }
    out
}

/// Gray+A → RGBA.
fn interleave<T: Copy>(v: &[T]) -> Vec<T> {
    let mut out = Vec::with_capacity(v.len() * 2);
    for px in v.as_chunks::<2>().0 {
        out.extend_from_slice([px[0], px[0], px[0], px[1]].as_slice());
    }
    out
}

/// Naive CMYK → RGB (no ICC): R = (1-c)(1-k). TIFF stores true CMYK, unlike
/// Adobe JPEG's inverted convention.
fn cmyk_to_rgb8(v: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 3 / 4);
    for px in v.as_chunks::<4>().0 {
        let c = f64::from(px[0]) / 255.0;
        let m = f64::from(px[1]) / 255.0;
        let y = f64::from(px[2]) / 255.0;
        let k = f64::from(px[3]) / 255.0;
        for x in [c, m, y] {
            out.push((((1.0 - x) * (1.0 - k)) * 255.0).round() as u8);
        }
    }
    out
}

fn cmyk_to_rgb16(v: &[u16]) -> Vec<u16> {
    let max = 65535.0;
    let mut out = Vec::with_capacity(v.len() * 3 / 4);
    for px in v.as_chunks::<4>().0 {
        let c = f64::from(px[0]) / max;
        let m = f64::from(px[1]) / max;
        let y = f64::from(px[2]) / max;
        let k = f64::from(px[3]) / max;
        for x in [c, m, y] {
            out.push((((1.0 - x) * (1.0 - k)) * max).round() as u16);
        }
    }
    out
}
