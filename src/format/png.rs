//! PNG decode: cICP/iCCP/eXIf metadata + 8/16-bit pixels.

use super::png_text::scan_text_chunks;
use crate::domain::{
    Cicp, ColorDesc, CustomColor, Decoded, DecodedImage, Hdr, ImageFrame, MetaBoxes, Orientation,
    Pixels, strip_exif_prefix,
};

pub fn decode(data: &[u8]) -> Result<Decoded, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    // EXPAND: palette → RGB, grayscale <8bit → 8bit.
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("PNG header: {e}"))?;
    let info = reader.info().clone();
    // animation_control present ⇒ APNG; we only handle stills.
    if info.animation_control.is_some() {
        return Err("animated PNG (APNG) not supported (stills only)".into());
    }
    // IHDR dimensions are attacker-controlled: bound the decode buffer.
    const MAX_BUFFER_BYTES: u64 = 1 << 32;

    let (color_type, bit_depth) = reader.output_color_type();
    let width = info.width;
    let height = info.height;
    let bpp = match (color_type, bit_depth) {
        (png::ColorType::Grayscale, png::BitDepth::Eight) => 1,
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => 2,
        (png::ColorType::Rgb, png::BitDepth::Eight) => 3,
        (png::ColorType::Rgba, png::BitDepth::Eight) => 4,
        (png::ColorType::Grayscale, png::BitDepth::Sixteen) => 2,
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Sixteen) => 4,
        (png::ColorType::Rgb, png::BitDepth::Sixteen) => 6,
        (png::ColorType::Rgba, png::BitDepth::Sixteen) => 8,
        other => return Err(format!("unsupported PNG output type {other:?}")),
    };
    // Budget the post-expansion buffer too: Gray/GA grow to 3/4 samples,
    // allocated while the decode buffer is still alive.
    let expand: u32 = match color_type {
        png::ColorType::Grayscale => 3,
        png::ColorType::GrayscaleAlpha => 2,
        _ => 1,
    };
    let buf_len = u64::from(width) * u64::from(height) * u64::from(bpp as u32);
    if buf_len * u64::from(expand) > MAX_BUFFER_BYTES {
        return Err(format!("PNG dimensions too large ({width}x{height})"));
    }
    let mut buf = vec![0u8; buf_len as usize];
    reader
        .next_frame(&mut buf)
        .map_err(|e| format!("PNG decode: {e}"))?;

    let (pixels, has_alpha) = match bit_depth {
        png::BitDepth::Eight => {
            let (px, alpha) = expand_to_rgb8(buf, color_type);
            (Pixels::U8(px), alpha)
        }
        png::BitDepth::Sixteen => {
            let (px, alpha) = expand_to_rgb16(&buf, color_type);
            (Pixels::U16(px), alpha)
        }
        d => return Err(format!("unsupported PNG bit depth {d:?}")),
    };

    let cicp = info.coding_independent_code_points.map(|c| Cicp {
        color_primaries: u16::from(c.color_primaries),
        transfer_characteristics: u16::from(c.transfer_function),
    });
    let hdr = Hdr::for_transfer(
        cicp.as_ref()
            .map(|c| c.transfer_characteristics)
            .unwrap_or(0),
    );

    let text = scan_text_chunks(data);
    let exif = info
        .exif_metadata
        .as_deref()
        .map(<[u8]>::to_vec)
        .or(text.raw_exif)
        .map(strip_exif_prefix);
    let orientation = Orientation::from_exif(exif.as_deref()).unwrap_or_default();

    let frame = ImageFrame {
        width,
        height,
        bit_depth: match bit_depth {
            png::BitDepth::Eight => 8,
            _ => 16,
        },
        has_alpha,
        alpha_premultiplied: false,
        pixels,
    };
    Ok(Decoded::Image(Box::new(
        DecodedImage::new(
            frame,
            ColorDesc::new(cicp, info.icc_profile.as_deref().map(<[u8]>::to_vec))
                .custom(gama_chrm(&info)),
            MetaBoxes::new(exif, text.xmp),
        )
        .orientation(orientation)
        .hdr(hdr),
    )))
}

/// gAMA/cHRM describe the color only when no cICP, iCCP or sRGB chunk
/// does (PNG spec precedence).
fn gama_chrm(info: &png::Info) -> Option<CustomColor> {
    if info.coding_independent_code_points.is_some()
        || info.icc_profile.is_some()
        || info.srgb.is_some()
    {
        return None;
    }
    let xy = |(x, y): (png::ScaledFloat, png::ScaledFloat)| {
        [f64::from(x.into_value()), f64::from(y.into_value())]
    };
    let gamma = info.gama_chunk.map(|g| f64::from(g.into_value()));
    let gamma = gamma.filter(|g| {
        let ok = (MIN_OETF_GAMMA..=1.0).contains(g);
        if !ok {
            eprintln!("note: PNG gAMA {g} out of range, ignored");
        }
        ok
    });
    let chromaticities = info
        .chrm_chunk
        .map(|c| [xy(c.white), xy(c.red), xy(c.green), xy(c.blue)])
        .filter(|c| {
            let ok = chromaticities_ok(c);
            if !ok {
                eprintln!("note: PNG cHRM invalid or degenerate, ignored");
            }
            ok
        });
    let custom = CustomColor {
        gamma,
        chromaticities,
    };
    (custom.gamma.is_some() || custom.chromaticities.is_some()).then_some(custom)
}

/// Smallest OETF exponent libjxl accepts (its gamma is stored as 1e7/x).
const MIN_OETF_GAMMA: f64 = 1.0 / 8192.0;
/// Minimum |determinant| of the primaries' xyY matrix: below it the
/// triangle is degenerate and libjxl fails late, in ProcessOutput.
const MIN_PRIMARIES_DET: f64 = 1e-6;

/// Each xy inside the unit triangle and the RGB primaries non-collinear.
fn chromaticities_ok(c: &[[f64; 2]; 4]) -> bool {
    let in_gamut = c.iter().all(|&[x, y]| x > 0.0 && y > 0.0 && x + y <= 1.0);
    let [_, [rx, ry], [gx, gy], [bx, by]] = *c;
    let det = rx * (gy - by) - gx * (ry - by) + bx * (ry - gy);
    in_gamut && det.abs() > MIN_PRIMARIES_DET
}

fn expand_to_rgb8(buf: Vec<u8>, ct: png::ColorType) -> (Vec<u8>, bool) {
    match ct {
        png::ColorType::Rgb => (buf, false),
        png::ColorType::Rgba => (buf, true),
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(buf.len() * 3);
            for &g in &buf {
                out.extend_from_slice(&[g, g, g]);
            }
            (out, false)
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(buf.len() * 2);
            for px in buf.as_chunks::<2>().0 {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            (out, true)
        }
        // Palette never survives EXPAND; anything else passes through as-is.
        _ => (buf, false),
    }
}

/// PNG 16-bit samples are big-endian; the encoder wants native u16.
/// Converted while expanding, so no second full-size buffer is alive.
fn expand_to_rgb16(buf: &[u8], ct: png::ColorType) -> (Vec<u16>, bool) {
    let samples = buf
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c));
    match ct {
        png::ColorType::Grayscale => (samples.flat_map(|g| [g, g, g]).collect(), false),
        png::ColorType::GrayscaleAlpha => {
            let px = buf.as_chunks::<4>().0.iter().flat_map(|&[g0, g1, a0, a1]| {
                let g = u16::from_be_bytes([g0, g1]);
                [g, g, g, u16::from_be_bytes([a0, a1])]
            });
            (px.collect(), true)
        }
        png::ColorType::Rgba => (samples.collect(), true),
        _ => (samples.collect(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromaticities_reject_zero_and_degenerate() {
        let srgb = [[0.3127, 0.329], [0.64, 0.33], [0.3, 0.6], [0.15, 0.06]];
        assert!(chromaticities_ok(&srgb));
        assert!(!chromaticities_ok(&[[0.0; 2]; 4]));
        assert!(!chromaticities_ok(&[[0.3127, 0.329]; 4]));
        assert!(!chromaticities_ok(&[[30.0, 30.0]; 4]));
    }

    #[test]
    fn sixteen_bit_gray_alpha_expands_big_endian() {
        let (px, alpha) =
            expand_to_rgb16(&[0x12, 0x34, 0xAB, 0xCD], png::ColorType::GrayscaleAlpha);
        assert!(alpha);
        assert_eq!(px, [0x1234, 0x1234, 0x1234, 0xABCD]);
        let (px, _) = expand_to_rgb16(&[0x00, 0x01], png::ColorType::Grayscale);
        assert_eq!(px, [1, 1, 1]);
    }
}
