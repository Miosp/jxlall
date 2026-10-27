//! TIFF decode: multipage → one Decoded per page; ICC tag; 8/16-bit;
//! float → F32; CMYK/YCbCr → RGB; planar → interleaved.

use crate::domain::{
    ColorDesc, Decoded, DecodedImage, ImageFrame, MetaBoxes, Orientation, Pixels,
};
use tiff::tags::Tag;

/// Per-page pixel budget (268M px ≈ 8k×33k; RGBA16 ≈ 2 GiB).
const MAX_PAGE_PIXELS: u64 = 1 << 28;
/// Aggregate decoded-frame bytes across pages, bounding the multi-page bomb
/// (small under test).
const MAX_TOTAL_BYTES: u64 = if cfg!(test) { 1 << 20 } else { 4 << 30 };
/// Decoded page size limit for the worst case, RGBA16 (8 B/px).
const MAX_PAGE_BYTES: usize = (MAX_PAGE_PIXELS * 8) as usize;
/// ExtraSamples values.
const ASSOCIATED_ALPHA: u16 = 1;
const UNASSOCIATED_ALPHA: u16 = 2;
const COMPRESSION_JPEG: u16 = 7;
const TAG_XMP: u16 = 700;
const TAG_YCBCR_COEFFICIENTS: u16 = 529;
const TAG_REFERENCE_BLACK_WHITE: u16 = 532;
/// BT.601 luma weights, the TIFF default for YCbCrCoefficients.
const BT601_LUMA: [f64; 3] = [0.299, 0.587, 0.114];
/// libtiff's default ReferenceBlackWhite for 8-bit YCbCr (full range).
const FULL_RANGE_REF_BW: [f64; 6] = [0.0, 255.0, 128.0, 255.0, 128.0, 255.0];

/// Per-page sample layout facts beyond `ColorType`.
struct Layout {
    planes: usize,
    associated_alpha: bool,
    ycbcr: YCbCr,
}

struct YCbCr {
    luma: [f64; 3],
    ref_bw: [f64; 6],
}

impl YCbCr {
    const JFIF: Self = Self { luma: BT601_LUMA, ref_bw: FULL_RANGE_REF_BW };
}

pub fn decode(data: &std::sync::Arc<Vec<u8>>) -> Result<Vec<Decoded>, String> {
    // Crate defaults (256 MiB) would reject pages our own budget allows.
    let mut limits = tiff::decoder::Limits::default();
    limits.decoding_buffer_size = MAX_PAGE_BYTES;
    limits.intermediate_buffer_size = 512 * 1024 * 1024;
    let mut dec = tiff::decoder::Decoder::new(std::io::Cursor::new(data.as_slice()))
        .map_err(|e| format!("TIFF open: {e}"))?
        .with_limits(limits);
    let mut total_bytes = 0u64;
    let mut pages = Vec::new();

    loop {
        let (w, h) = dec.dimensions().map_err(|e| format!("TIFF dims: {e}"))?;
        let ct = dec.colortype().map_err(|e| format!("TIFF colortype: {e}"))?;
        let pixels = u64::from(w) * u64::from(h);
        if pixels > MAX_PAGE_PIXELS {
            return Err(format!("TIFF page too large ({w}x{h})"));
        }
        let icc = find_bytes(&mut dec, Tag::IccProfile, "ICC profile");
        let orientation = dec
            .find_tag(Tag::Orientation)
            .ok()
            .flatten()
            .and_then(|v| v.into_u32().ok())
            .and_then(|v| u8::try_from(v).ok())
            .map(Orientation::new)
            .unwrap_or_default();

        let extra = dec
            .find_tag_unsigned_vec::<u16>(Tag::ExtraSamples)
            .ok()
            .flatten()
            .unwrap_or_default();
        // The crate reports gray+alpha as two-sample Multiband.
        let ct = match (ct, extra.first()) {
            (
                tiff::ColorType::Multiband { bit_depth, num_samples: 2 },
                Some(&(ASSOCIATED_ALPHA | UNASSOCIATED_ALPHA)),
            ) => tiff::ColorType::GrayA(bit_depth),
            (ct, _) => ct,
        };
        let ycbcr = match ct {
            tiff::ColorType::YCbCr(_) => ycbcr_params(&mut dec),
            _ => YCbCr::JFIF,
        };

        // Over the limit, the crate would silently read only the first plane.
        let complete_len = dec
            .image_buffer_layout()
            .map_err(|e| format!("TIFF decode: {e}"))?
            .complete_len;
        if complete_len > MAX_PAGE_BYTES {
            return Err("TIFF page too large".into());
        }
        if total_bytes + complete_len as u64 > MAX_TOTAL_BYTES {
            return Err("TIFF total decoded size too large".into());
        }
        let mut img = tiff::decoder::DecodingResult::U8(Vec::new());
        let buffer = dec
            .read_image_to_buffer(&mut img)
            .map_err(|e| format!("TIFF decode: {e}"))?;
        let layout = Layout {
            planes: buffer.planes,
            associated_alpha: extra.first() == Some(&ASSOCIATED_ALPHA),
            ycbcr,
        };
        let frame = frame_from(w, h, ct, img, &layout)?;
        // Gray expansion and sub-8-bit unpacking grow the frame past `complete_len`.
        total_bytes += frame.pixels.as_raw_parts().1 as u64;
        if total_bytes > MAX_TOTAL_BYTES {
            return Err("TIFF total decoded size too large".into());
        }

        let exif = dec.ifd_pointer().and_then(|ifd| {
            super::tiff_exif::minimal_exif(data, ifd.0)
                .unwrap_or_else(|e| {
                    eprintln!("note: malformed TIFF metadata dropped ({e})");
                    None
                })
        });
        let meta = MetaBoxes::new(exif, find_bytes(&mut dec, Tag::Unknown(TAG_XMP), "XMP"));
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
    layout: &Layout,
) -> Result<ImageFrame, String> {
    use tiff::ColorType;
    use tiff::decoder::DecodingResult;
    // Sub-8-bit samples arrive bit-packed (MSB-first, per row): expand to
    // full-range 8-bit before the sample-layout conversions below. Planar
    // images are unpacked plane by plane, then interleaved.
    let bits = ct.bit_depth();
    let planes = layout.planes;
    let img = match ((1..8).contains(&bits), img) {
        (true, DecodingResult::U8(v)) if planes > 1 => {
            let mut unpacked = Vec::new();
            for plane in split_planes(&v, planes)? {
                unpacked.extend(unpack_bits(plane, w, h, 1, bits)?);
            }
            DecodingResult::U8(interleave_planes(&unpacked, planes)?)
        }
        (true, DecodingResult::U8(v)) => {
            DecodingResult::U8(unpack_bits(&v, w, h, samples_per_pixel(ct), bits)?)
        }
        (_, img) if planes == 1 => img,
        (_, DecodingResult::U8(v)) => DecodingResult::U8(interleave_planes(&v, planes)?),
        (_, DecodingResult::U16(v)) => DecodingResult::U16(interleave_planes(&v, planes)?),
        (_, DecodingResult::F32(v)) => DecodingResult::F32(interleave_planes(&v, planes)?),
        (_, img) => img,
    };
    let frame = |bit_depth, has_alpha, pixels| ImageFrame {
        width: w,
        height: h,
        bit_depth,
        has_alpha,
        alpha_premultiplied: has_alpha && layout.associated_alpha,
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
        (ColorType::YCbCr(8), DecodingResult::U8(v)) => {
            frame(8, false, Pixels::U8(ycbcr_to_rgb8(&v, &layout.ycbcr)))
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

/// Planes of `v`, each holding the same number of samples.
fn split_planes<T>(v: &[T], planes: usize) -> Result<std::slice::ChunksExact<'_, T>, String> {
    if planes == 0 || v.is_empty() || !v.len().is_multiple_of(planes) {
        return Err("TIFF planar buffer size mismatch".into());
    }
    Ok(v.chunks_exact(v.len() / planes))
}

/// Planar (RRR…GGG…BBB…) → interleaved (RGBRGB…).
fn interleave_planes<T: Copy>(v: &[T], planes: usize) -> Result<Vec<T>, String> {
    let planes: Vec<&[T]> = split_planes(v, planes)?.collect();
    let mut out = Vec::with_capacity(v.len());
    for i in 0..planes[0].len() {
        out.extend(planes.iter().map(|p| p[i]));
    }
    Ok(out)
}

/// YCbCr conversion parameters. JPEG-compressed data follows JFIF (BT.601,
/// full range) regardless of the tags, as libtiff's JPEGCOLORMODE_RGB does.
fn ycbcr_params<R: std::io::Read + std::io::Seek>(
    dec: &mut tiff::decoder::Decoder<R>,
) -> YCbCr {
    let default = YCbCr::JFIF;
    if dec.find_tag_unsigned::<u16>(Tag::Compression).ok().flatten() == Some(COMPRESSION_JPEG) {
        return default;
    }
    let mut rationals = |tag| -> Option<Vec<f64>> {
        let list = match dec.find_tag(Tag::Unknown(tag)).ok()?? {
            tiff::decoder::ifd::Value::List(l) => l,
            v => vec![v],
        };
        list.into_iter()
            .map(|v| match v {
                tiff::decoder::ifd::Value::Rational(n, d) if d != 0 => {
                    Some(f64::from(n) / f64::from(d))
                }
                _ => None,
            })
            .collect()
    };
    let luma = rationals(TAG_YCBCR_COEFFICIENTS)
        .and_then(|v| <[f64; 3]>::try_from(v).ok())
        .filter(|l| l[1] > 0.0)
        .unwrap_or(default.luma);
    let ref_bw = rationals(TAG_REFERENCE_BLACK_WHITE)
        .and_then(|v| <[f64; 6]>::try_from(v).ok())
        .filter(|r| r[1] != r[0] && r[3] != r[2] && r[5] != r[4])
        .unwrap_or(default.ref_bw);
    YCbCr { luma, ref_bw }
}

/// TIFF 6.0 section 21 YCbCr → RGB (green from all three luma weights).
fn ycbcr_to_rgb8(v: &[u8], p: &YCbCr) -> Vec<u8> {
    let [lr, lg, lb] = p.luma;
    let r = p.ref_bw;
    let mut out = Vec::with_capacity(v.len());
    for px in v.as_chunks::<3>().0 {
        let y = (f64::from(px[0]) - r[0]) * 255.0 / (r[1] - r[0]);
        let cb = (f64::from(px[1]) - r[2]) * 127.0 / (r[3] - r[2]);
        let cr = (f64::from(px[2]) - r[4]) * 127.0 / (r[5] - r[4]);
        let red = y + cr * (2.0 - 2.0 * lr);
        let blue = y + cb * (2.0 - 2.0 * lb);
        let green = (y - lb * blue - lr * red) / lg;
        for c in [red, green, blue] {
            out.push(c.round().clamp(0.0, 255.0) as u8);
        }
    }
    out
}

/// Byte-valued tag (ICC, XMP); an unreadable one is dropped with a note.
fn find_bytes<R: std::io::Read + std::io::Seek>(
    dec: &mut tiff::decoder::Decoder<R>,
    tag: Tag,
    what: &str,
) -> Option<Vec<u8>> {
    match dec.find_tag(tag).and_then(|v| v.map(|v| v.into_u8_vec()).transpose()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("note: unreadable TIFF {what} tag dropped ({e})");
            None
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    enum V {
        Short(Vec<u16>),
        Long(Vec<u32>),
        Ascii(&'static str),
        Rational(Vec<u32>),
        Bytes(&'static [u8]),
        Ifd(Vec<(u16, V)>),
    }

    /// Append an IFD (sub-IFDs and out-of-line values first); returns its offset.
    fn write_ifd(out: &mut Vec<u8>, entries: Vec<(u16, V)>) -> u32 {
        let mut table = Vec::new();
        for (tag, v) in entries {
            let (ty, count, bytes): (u16, usize, Vec<u8>) = match v {
                V::Short(s) => (3, s.len(), s.iter().flat_map(|x| x.to_le_bytes()).collect()),
                V::Long(s) => (4, s.len(), s.iter().flat_map(|x| x.to_le_bytes()).collect()),
                V::Ascii(a) => (2, a.len() + 1, [a.as_bytes(), &[0]].concat()),
                V::Bytes(b) => (1, b.len(), b.to_vec()),
                // Flattened numerator/denominator pairs.
                V::Rational(r) => (5, r.len() / 2, r.iter().flat_map(|x| x.to_le_bytes()).collect()),
                V::Ifd(sub) => (4, 1, write_ifd(out, sub).to_le_bytes().to_vec()),
            };
            let value = if bytes.len() <= 4 {
                let mut v = bytes.clone();
                v.resize(4, 0);
                v
            } else {
                let off = out.len() as u32;
                out.extend_from_slice(&bytes);
                if out.len() % 2 == 1 {
                    out.push(0);
                }
                off.to_le_bytes().to_vec()
            };
            table.push((tag, ty, count as u32, value));
        }
        table.sort_by_key(|e| e.0);
        let off = out.len() as u32;
        out.extend_from_slice(&(table.len() as u16).to_le_bytes());
        for (tag, ty, count, value) in table {
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&ty.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&value);
        }
        out.extend_from_slice(&0u32.to_le_bytes());
        off
    }

    /// IFD tags and one strip per plane.
    type Page = (Vec<(u16, V)>, Vec<Vec<u8>>);

    /// Little-endian classic TIFF.
    fn build_tiff(pages: Vec<Page>) -> Vec<u8> {
        let mut out = b"II*\0\0\0\0\0".to_vec();
        let mut next_ptr = 4;
        for (mut tags, strips) in pages {
            let mut offsets = Vec::new();
            for s in &strips {
                offsets.push(out.len() as u32);
                out.extend_from_slice(s);
                if out.len() % 2 == 1 {
                    out.push(0);
                }
            }
            tags.push((273, V::Long(offsets)));
            tags.push((279, V::Long(strips.iter().map(|s| s.len() as u32).collect())));
            let ifd = write_ifd(&mut out, tags);
            out[next_ptr..next_ptr + 4].copy_from_slice(&ifd.to_le_bytes());
            next_ptr = out.len() - 4;
        }
        out
    }

    fn image_tags(w: u32, h: u32, bps: Vec<u16>, photometric: u16, planar: u16) -> Vec<(u16, V)> {
        vec![
            (256, V::Long(vec![w])),
            (257, V::Long(vec![h])),
            (277, V::Short(vec![bps.len() as u16])),
            (258, V::Short(bps)),
            (259, V::Short(vec![1])),
            (262, V::Short(vec![photometric])),
            (278, V::Long(vec![h])),
            (284, V::Short(vec![planar])),
        ]
    }

    fn decode_pages(bytes: Vec<u8>) -> Vec<DecodedImage> {
        decode(&Arc::new(bytes))
            .unwrap()
            .into_iter()
            .map(|d| match d {
                Decoded::Image(img) => *img,
                Decoded::Jpeg(_) => panic!("unexpected JPEG"),
            })
            .collect()
    }

    fn u8_pixels(img: &DecodedImage) -> &[u8] {
        match &img.frame.pixels {
            Pixels::U8(v) => v,
            _ => panic!("expected u8 pixels"),
        }
    }

    const RGB: u16 = 2;
    const BLACK_IS_ZERO: u16 = 1;
    const YCBCR: u16 = 6;
    const CHUNKY: u16 = 1;
    const PLANAR: u16 = 2;

    #[test]
    fn planar_rgb_is_interleaved() {
        let tags = image_tags(2, 1, vec![8, 8, 8], RGB, PLANAR);
        let planes = vec![vec![10, 20], vec![30, 40], vec![50, 60]];
        let img = decode_pages(build_tiff(vec![(tags, planes)])).remove(0);
        assert_eq!(u8_pixels(&img), [10, 30, 50, 20, 40, 60]);
    }

    #[test]
    fn planar_rgb16_is_interleaved() {
        let tags = image_tags(1, 1, vec![16, 16, 16], RGB, PLANAR);
        let planes = vec![vec![1, 0], vec![2, 0], vec![3, 0]];
        let img = decode_pages(build_tiff(vec![(tags, planes)])).remove(0);
        assert!(matches!(&img.frame.pixels, Pixels::U16(v) if v == &[1, 2, 3]));
    }

    #[test]
    fn gray_alpha_expands_to_rgba() {
        let mut tags = image_tags(2, 1, vec![8, 8], BLACK_IS_ZERO, CHUNKY);
        tags.push((338, V::Short(vec![UNASSOCIATED_ALPHA])));
        let img = decode_pages(build_tiff(vec![(tags, vec![vec![100, 255, 50, 128]])])).remove(0);
        assert!(img.frame.has_alpha && !img.frame.alpha_premultiplied);
        assert_eq!(u8_pixels(&img), [100, 100, 100, 255, 50, 50, 50, 128]);
    }

    #[test]
    fn associated_alpha_is_premultiplied() {
        let mut tags = image_tags(1, 1, vec![8, 8, 8, 8], RGB, CHUNKY);
        tags.push((338, V::Short(vec![ASSOCIATED_ALPHA])));
        let img = decode_pages(build_tiff(vec![(tags, vec![vec![1, 2, 3, 4]])])).remove(0);
        assert!(img.frame.has_alpha && img.frame.alpha_premultiplied);

        let mut tags = image_tags(1, 1, vec![8, 8], BLACK_IS_ZERO, CHUNKY);
        tags.push((338, V::Short(vec![ASSOCIATED_ALPHA])));
        let img = decode_pages(build_tiff(vec![(tags, vec![vec![1, 2]])])).remove(0);
        assert!(img.frame.alpha_premultiplied);
    }

    #[test]
    fn ycbcr_converts_to_rgb() {
        let mut tags = image_tags(2, 1, vec![8, 8, 8], YCBCR, CHUNKY);
        tags.push((530, V::Short(vec![1, 1])));
        // White, and BT.601 full-range red.
        let img = decode_pages(build_tiff(vec![(tags, vec![vec![255, 128, 128, 76, 85, 255]])]))
            .remove(0);
        let expected = [255, 255, 255, 254, 0, 0];
        for (got, want) in u8_pixels(&img).iter().zip(expected) {
            assert!(got.abs_diff(want) <= 1, "{:?}", u8_pixels(&img));
        }
    }

    #[test]
    fn ycbcr_honours_reference_black_white() {
        let mut tags = image_tags(1, 1, vec![8, 8, 8], YCBCR, CHUNKY);
        tags.push((530, V::Short(vec![1, 1])));
        // Studio range: Y 16..235, chroma 128 ± 112.
        tags.push((532, V::Rational(vec![16, 1, 235, 1, 128, 1, 240, 1, 128, 1, 240, 1])));
        let img = decode_pages(build_tiff(vec![(tags, vec![vec![235, 128, 128]])])).remove(0);
        assert_eq!(u8_pixels(&img), [255, 255, 255]);
    }

    #[test]
    fn planar_one_bit_gray_alpha() {
        let mut tags = image_tags(8, 1, vec![1, 1], BLACK_IS_ZERO, PLANAR);
        tags.push((338, V::Short(vec![UNASSOCIATED_ALPHA])));
        let img = decode_pages(build_tiff(vec![(tags, vec![vec![0b1010_0000], vec![0b1100_0000]])]))
            .remove(0);
        let px = u8_pixels(&img);
        let gray: Vec<u8> = px.iter().step_by(4).copied().collect();
        let alpha: Vec<u8> = px.iter().skip(3).step_by(4).copied().collect();
        assert_eq!(gray, [255, 0, 255, 0, 0, 0, 0, 0]);
        assert_eq!(alpha, [255, 255, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn subsampled_uncompressed_ycbcr_is_rejected() {
        // No YCbCrSubSampling tag: the TIFF default is 2x2.
        let tags = image_tags(2, 2, vec![8, 8, 8], YCBCR, CHUNKY);
        let Err(e) = decode(&Arc::new(build_tiff(vec![(tags, vec![vec![0; 6]])]))) else {
            panic!("subsampled YCbCr accepted");
        };
        assert!(e.contains("subsampling"), "{e}");
    }

    fn exif_page(make: &'static str, pixel: u8) -> Page {
        let mut tags = image_tags(64, 64, vec![8], BLACK_IS_ZERO, CHUNKY);
        tags.extend([
            (271, V::Ascii(make)),
            (272, V::Ascii("Model Name")),
            (274, V::Short(vec![6])),
            (34665, V::Ifd(vec![
                (36867, V::Ascii("2024:01:02 03:04:05")),
                (40965, V::Ifd(vec![(1, V::Ascii("R98"))])),
            ])),
            (34853, V::Ifd(vec![(1, V::Ascii("N"))])),
        ]);
        (tags, vec![vec![pixel; 64 * 64]])
    }

    fn field(exif: &exif::Exif, tag: exif::Tag) -> String {
        exif.get_field(tag, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
            .unwrap_or_default()
    }

    #[test]
    fn exif_is_minimal_per_page() {
        let src = build_tiff(vec![exif_page("One", 1), exif_page("Two", 2)]);
        let pages = decode_pages(src.clone());
        for (img, make) in pages.iter().zip(["\"One\"", "\"Two\""]) {
            let payload = img.meta.exif.as_deref().expect("exif kept").clone();
            assert!(payload.len() < 64 * 64, "payload {} bytes", payload.len());
            assert_eq!(Orientation::from_exif(Some(&payload)), Some(Orientation::new(6)));
            let exif = exif::Reader::new().read_raw(payload).unwrap();
            assert_eq!(field(&exif, exif::Tag::Make), make);
            assert_eq!(field(&exif, exif::Tag::Model), "\"Model Name\"");
            assert_eq!(field(&exif, exif::Tag::DateTimeOriginal), "2024-01-02 03:04:05");
            assert_eq!(field(&exif, exif::Tag::GPSLatitudeRef), "N");
            assert!(exif.get_field(exif::Tag::InteroperabilityIndex, exif::In::PRIMARY).is_some());
            for structural in [exif::Tag::StripOffsets, exif::Tag::ImageWidth, exif::Tag::BitsPerSample] {
                assert!(exif.get_field(structural, exif::In::PRIMARY).is_none());
            }
            assert!(exif.fields().all(|f| f.ifd_num == exif::In::PRIMARY));
        }
    }

    #[test]
    fn zero_width_page_is_rejected() {
        let zero = || (image_tags(0, 1, vec![1], BLACK_IS_ZERO, CHUNKY), vec![vec![0]]);
        let one = || (image_tags(1, 1, vec![1], BLACK_IS_ZERO, CHUNKY), vec![vec![0]]);
        for pages in [vec![zero()], vec![one(), zero()]] {
            let Err(e) = decode(&Arc::new(build_tiff(pages))) else {
                panic!("zero-width page accepted");
            };
            assert!(e.contains("dimensions"), "{e}");
        }
    }

    #[test]
    fn total_budget_counts_bytes() {
        // Each page is 1.5 MiB as RGB16 but only 256K pixels.
        let page = || (image_tags(512, 512, vec![16, 16, 16], RGB, CHUNKY), vec![vec![0; 512 * 512 * 6]]);
        let pages = (0..MAX_TOTAL_BYTES.div_ceil(512 * 512 * 6) as usize + 1).map(|_| page()).collect();
        let Err(e) = decode(&Arc::new(build_tiff(pages))) else {
            panic!("byte budget not enforced");
        };
        assert!(e.contains("total"), "{e}");
    }

    #[test]
    fn xmp_goes_to_xml_box_not_exif() {
        const XMP: &[u8] = b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>";
        let mut tags = image_tags(1, 1, vec![8], BLACK_IS_ZERO, CHUNKY);
        tags.push((271, V::Ascii("Make")));
        tags.push((700, V::Bytes(XMP)));
        let img = decode_pages(build_tiff(vec![(tags, vec![vec![7]])])).remove(0);
        assert_eq!(img.meta.xmp.as_deref().map(Vec::as_slice), Some(XMP));
        let payload = img.meta.exif.as_deref().expect("exif kept").clone();
        let exif = exif::Reader::new().read_raw(payload).unwrap();
        assert!(exif.get_field(exif::Tag(exif::Context::Tiff, 700), exif::In::PRIMARY).is_none());
    }

    #[test]
    fn page_without_descriptive_tags_has_no_exif() {
        let tags = image_tags(1, 1, vec![8], BLACK_IS_ZERO, CHUNKY);
        let img = decode_pages(build_tiff(vec![(tags, vec![vec![7]])])).remove(0);
        assert!(img.meta.exif.is_none());
    }

    #[test]
    fn malformed_exif_pointer_drops_exif_only() {
        let mut tags = image_tags(1, 1, vec![8], BLACK_IS_ZERO, CHUNKY);
        tags.push((271, V::Ascii("Make")));
        tags.push((34665, V::Long(vec![0xFFFF_FF00])));
        let img = decode_pages(build_tiff(vec![(tags, vec![vec![7]])])).remove(0);
        assert!(img.meta.exif.is_none());
        assert_eq!(u8_pixels(&img), [7, 7, 7]);
    }

    #[test]
    fn bigtiff_exif_becomes_classic() {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut enc = tiff::encoder::TiffEncoder::new_big(&mut buf).unwrap();
        let mut image = enc.new_image::<tiff::encoder::colortype::Gray8>(1, 1).unwrap();
        image.encoder().write_tag(Tag::Make, "Big Camera").unwrap();
        image.encoder().write_tag(Tag::Orientation, 3u16).unwrap();
        // 8 bytes: inline in BigTIFF, out of line in classic TIFF.
        image.x_resolution(tiff::encoder::Rational { n: 300, d: 1 });
        image.write_data(&[9]).unwrap();
        let img = decode_pages(buf.into_inner()).remove(0);
        let payload = img.meta.exif.as_deref().expect("exif kept").clone();
        assert_eq!(&payload[..4], b"II*\0");
        let exif = exif::Reader::new().read_raw(payload).unwrap();
        assert_eq!(field(&exif, exif::Tag::Make), "\"Big Camera\"");
        assert_eq!(field(&exif, exif::Tag::XResolution), "300");
        assert_eq!(img.orientation, Orientation::new(3));
    }
}
