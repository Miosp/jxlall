//! PNG decode: cICP/iCCP/eXIf metadata + 8/16-bit pixels.

use crate::domain::{
    Cicp, ColorDesc, Decoded, DecodedImage, Hdr, ImageFrame, MetaBoxes, Orientation, Pixels,
};
use super::png_text::scan_text_chunks;

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
            // PNG 16-bit output is big-endian; the encoder wants native u16.
            let native: Vec<u16> = buf
                .as_chunks::<2>().0.iter()
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            let (px, alpha) = expand_to_rgb16(native, color_type);
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
        .or(text.raw_exif);
    let orientation = Orientation::from_exif(exif.as_deref()).unwrap_or_default();

    let frame = ImageFrame {
        width,
        height,
        bit_depth: match bit_depth {
            png::BitDepth::Eight => 8,
            _ => 16,
        },
        has_alpha,
        pixels,
    };
    Ok(Decoded::Image(Box::new(
        DecodedImage::new(
            frame,
            ColorDesc::new(cicp, info.icc_profile.as_deref().map(<[u8]>::to_vec)),
            MetaBoxes::new(exif, text.xmp),
        )
        .orientation(orientation)
        .hdr(hdr),
    )))
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

fn expand_to_rgb16(buf: Vec<u16>, ct: png::ColorType) -> (Vec<u16>, bool) {
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
        _ => (buf, false),
    }
}
