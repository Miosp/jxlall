//! HEIF/HEIC/AVIF decode via `heic` (std + av1 features).

use crate::domain::{
    Cicp, ColorDesc, Decoded, DecodedImage, GainMapData, Hdr, ImageFrame, MetaBoxes, Orientation,
    Pixels, reset_exif_orientation, strip_exif_prefix,
};

pub fn decode(data: &[u8]) -> Result<Decoded, String> {
    let mut info = heic::ImageInfo::from_bytes(data).map_err(|e| format!("HEIF probe: {e}"))?;

    let cicp = (info.color_primaries != 0 || info.transfer_characteristics != 0).then_some(Cicp {
        color_primaries: info.color_primaries,
        transfer_characteristics: info.transfer_characteristics,
    });
    let hdr = Hdr::for_transfer(info.transfer_characteristics);
    // heic already applies irot/imir to the pixels; HEIF treats those as
    // authoritative, so EXIF Orientation must not rotate a second time.
    // The source orientation proxies the primary item's irot/imir, which
    // heic does not expose (used to vet the gain map's alignment).
    let source_orientation = Orientation::from_exif(info.exif.as_deref()).unwrap_or_default();
    let orientation = Orientation::new(1);
    let exif = std::mem::take(&mut info.exif).map(|e| {
        let mut e = strip_exif_prefix(e);
        reset_exif_orientation(&mut e);
        e
    });

    // 8-bit: one decode straight to the output layout. Deeper: decode the
    // YUV frame and convert (heic offers no 16-bit output layout).
    let cfg = heic::DecoderConfig::new();
    let (pixels, bit_depth, w, h) = if info.bit_depth <= 8 {
        let layout = if info.has_alpha {
            heic::PixelLayout::Rgba8
        } else {
            heic::PixelLayout::Rgb8
        };
        let out = cfg
            .decode(data, layout)
            .map_err(|e| format!("HEIF decode: {e}"))?;
        (Pixels::U8(out.data), 8, out.width, out.height)
    } else {
        let frame = cfg
            .decode_to_frame(data)
            .map_err(|e| format!("HEIF decode: {e}"))?;
        let (w, h) = (frame.cropped_width(), frame.cropped_height());
        let px = frame_to_rgb16(&frame, info.has_alpha);
        // Declaring the true depth saves libjxl from coding a sparse 16-bit
        // range; heic's full-range samples round back to it exactly. The
        // frame's depth set their scale; the container's may disagree.
        (Pixels::U16(px), frame.bit_depth, w, h)
    };

    let gain_map = decode_gain_map(data, &info, source_orientation, w, h)?;

    let frame = ImageFrame {
        width: w,
        height: h,
        bit_depth,
        has_alpha: info.has_alpha,
        alpha_premultiplied: false,
        pixels,
    };
    Ok(Decoded::Image(Box::new(
        DecodedImage::new(
            frame,
            ColorDesc::new(cicp, std::mem::take(&mut info.icc_profile)),
            MetaBoxes::new(exif, std::mem::take(&mut info.xmp)),
        )
        .orientation(orientation)
        .hdr(hdr)
        .gain_map(gain_map),
    )))
}

/// YCbCr -> RGB(A) for frames deeper than 8 bits, at the frame's own
/// depth, scaled to u16 so that the depth rounds back exactly.
///
/// Replaces heic 0.1.6 `to_rgb16`/`to_rgba16`, whose fixed-point shifts are
/// only right for 8-bit: at 10 bits full-range chroma comes out 4x too weak
/// and limited-range output 4x too dark (16x at 12 bits).
fn frame_to_rgb16(f: &heic::DecodedFrame, alpha: bool) -> Vec<u16> {
    let depth = u32::from(f.bit_depth);
    let max = ((1u32 << depth) - 1) as f32;
    let neutral = (1u32 << (depth - 1)) as f32;
    let scale = (1u32 << (depth - 8)) as f32;
    // H.273 normalization to E'y in [0, 1] and E'pb/E'pr in [-0.5, 0.5].
    let (y_off, y_div, c_div) = if f.full_range {
        (0.0, max, max)
    } else {
        (16.0 * scale, 219.0 * scale, 224.0 * scale)
    };
    let (kr, kb) = match f.matrix_coeffs {
        1 => (0.2126, 0.0722),
        9 | 10 => (0.2627, 0.0593),
        _ => (0.299, 0.114),
    };
    let identity = f.matrix_coeffs == 0;
    let kg = 1.0 - kr - kb;
    let (x0, x1) = (f.crop_left, f.width - f.crop_right);
    let (y0, y1) = (f.crop_top, f.height - f.crop_bottom);
    let (cw, cstride) = (f.width as usize, f.c_stride());
    let ch = if alpha { 4 } else { 3 };
    let mut out = Vec::with_capacity(((x1 - x0) * (y1 - y0)) as usize * ch);
    let quantize = |v: f32| -> u16 {
        let q = (v * max).round().clamp(0.0, max) as u32;
        ((q * 65535 + (1 << (depth - 1))) / ((1 << depth) - 1)) as u16
    };
    for y in y0..y1 {
        let cy = match f.chroma_format {
            1 => y / 2,
            _ => y,
        } as usize;
        for x in x0..x1 {
            let i = y as usize * cw + x as usize;
            let (cb, cr) = if f.chroma_format == 0 {
                (neutral, neutral)
            } else {
                let cx = if f.chroma_format == 3 { x } else { x / 2 } as usize;
                let ci = cy * cstride + cx;
                (
                    f32::from(f.cb_plane.get(ci).copied().unwrap_or(neutral as u16)),
                    f32::from(f.cr_plane.get(ci).copied().unwrap_or(neutral as u16)),
                )
            };
            let (r, g, b) = if identity {
                // GBR: Y carries G, Cb carries B, Cr carries R.
                let n = |v: f32| (v - y_off) / y_div;
                (n(cr), n(f32::from(f.y_plane[i])), n(cb))
            } else {
                let ey = (f32::from(f.y_plane[i]) - y_off) / y_div;
                let (pb, pr) = ((cb - neutral) / c_div, (cr - neutral) / c_div);
                let r = ey + 2.0 * (1.0 - kr) * pr;
                let b = ey + 2.0 * (1.0 - kb) * pb;
                (r, (ey - kr * r - kb * b) / kg, b)
            };
            out.extend([quantize(r), quantize(g), quantize(b)]);
            if alpha {
                let a = f
                    .alpha_plane
                    .as_ref()
                    .and_then(|a| a.get((y - y0) as usize * (x1 - x0) as usize + (x - x0) as usize))
                    .map_or(u16::MAX, |&a| quantize(f32::from(a) / max));
                out.push(a);
            }
        }
    }
    out
}

fn decode_gain_map(
    data: &[u8],
    info: &heic::ImageInfo,
    source_orientation: Orientation,
    w: u32,
    h: u32,
) -> Result<Option<GainMapData>, String> {
    if !info.has_gain_map {
        return Ok(None);
    }
    let g = match heic::DecoderConfig::new().decode_gain_map(data) {
        Ok(g) => g,
        Err(e) => {
            notice!("note: gain map decode failed ({e}), converting base image without it");
            return Ok(None);
        }
    };
    if !gain_map_aligned(source_orientation, (w, h), (g.width, g.height)) {
        notice!("note: gain map orientation differs from the base image, dropped");
        return Ok(None);
    }
    match crate::domain::from_heic(g) {
        Ok(g) => Ok(g),
        Err(e) => {
            notice!("note: gain map dropped ({e}), converting base image without it");
            Ok(None)
        }
    }
}

/// heic rotates the base by the primary item's irot/imir but decodes the
/// gain map with only its own transforms. Aligned only when known: no source
/// transform, or a 90°/transpose (5..=8) that the gain map's matching aspect
/// ratio proves it underwent too.
fn gain_map_aligned(source: Orientation, (w, h): (u32, u32), (gw, gh): (u32, u32)) -> bool {
    let rectangular = w != h && gw != gh;
    let same_aspect = (w > h) == (gw > gh);
    match source.get() {
        5..=8 => rectangular && same_aspect,
        o => o <= 1 && (!rectangular || same_aspect),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_bit_source_keeps_its_depth() {
        let Decoded::Image(img) = decode(include_bytes!("testdata/ten_bit.heic")).unwrap() else {
            panic!("expected pixels");
        };
        assert_eq!(img.frame.bit_depth, 10);
        assert!(matches!(img.frame.pixels, Pixels::U16(_)));
    }

    /// heic 0.1.6's own 16-bit conversion desaturated (full range) or
    /// darkened (limited range) every deeper-than-8-bit frame.
    #[test]
    fn ten_bit_avif_colors_are_right_in_both_ranges() {
        for data in [
            &include_bytes!("testdata/red10_full.avif")[..],
            &include_bytes!("testdata/red10_limited.avif")[..],
        ] {
            let Decoded::Image(img) = decode(data).unwrap() else {
                panic!("expected pixels");
            };
            let Pixels::U16(px) = &img.frame.pixels else {
                panic!("expected 16-bit samples");
            };
            let mid = (8 * 16 + 8) * 3;
            let [r, g, b] = [px[mid], px[mid + 1], px[mid + 2]];
            assert!(r > 64_500 && g < 1_000 && b < 1_000, "{r} {g} {b}");
        }
    }

    #[test]
    fn gain_map_alignment_rules() {
        let o = Orientation::new;
        assert!(gain_map_aligned(o(1), (40, 30), (20, 15)));
        assert!(!gain_map_aligned(o(1), (40, 30), (15, 20)));
        // Rotated base, gain map rotated too (aspect matches) vs not.
        assert!(gain_map_aligned(o(6), (30, 40), (15, 20)));
        assert!(!gain_map_aligned(o(6), (30, 40), (20, 15)));
        // Square: a rotation cannot be proven, flips never can.
        assert!(!gain_map_aligned(o(6), (32, 32), (16, 16)));
        assert!(!gain_map_aligned(o(3), (40, 30), (20, 15)));
        assert!(gain_map_aligned(Orientation::default(), (32, 32), (16, 16)));
    }
}
