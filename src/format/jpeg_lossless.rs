//! Lossless JPEG decode via jpeg-decoder (the only pure-Rust decoder).
//! It exposes no metadata API, so EXIF/XMP come from our marker scan.

use crate::domain::{
    ColorDesc, Decoded, DecodedImage, ImageFrame, MetaBoxes, Orientation, Pixels,
};
use super::jpeg_markers::{lossless_sof_params, scan_segments, MAX_PIXELS};

pub fn decode(data: &[u8]) -> Result<Decoded, String> {
    // jpeg-decoder handles 8-bit any-component and 2..=16-bit grayscale
    // lossless; anything else would silently decode wrong.
    if let Some((precision, components)) = lossless_sof_params(data)
        && precision != 8 && !(precision <= 16 && components == 1) {
            return Err(format!(
                "lossless JPEG with precision {precision} and {components} components not supported"
            ));
        }
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(data));
    dec.read_info()
        .map_err(|e| format!("lossless JPEG header: {e}"))?;
    let info = dec.info().ok_or("lossless JPEG: no info")?;
    if u64::from(info.width) * u64::from(info.height) > MAX_PIXELS {
        return Err(format!(
            "lossless JPEG too large ({}x{})",
            info.width, info.height
        ));
    }
    let pixels = dec
        .decode()
        .map_err(|e| format!("lossless JPEG decode: {e}"))?;

    use jpeg_decoder::PixelFormat;
    let (pixels, bit_depth) = match info.pixel_format {
        PixelFormat::RGB24 => (Pixels::U8(pixels), 8),
        PixelFormat::CMYK32 => {
            return Err("lossless CMYK JPEG not supported (no reference conversion)".into())
        }
        PixelFormat::L8 => {
            // jpeg-decoder returns NE u16 bytes for any precision != 8, so
            // sub-8-bit gray arrives as 2 bytes/sample: left-align into u8.
            let precision = lossless_sof_params(data).map(|(p, _)| p).unwrap_or(8);
            if precision == 8 {
                (Pixels::U8(expand_gray(&pixels)), 8)
            } else {
                let samples: Vec<u8> = pixels
                    .as_chunks::<2>().0.iter()
                    .map(|c| (u16::from_ne_bytes([c[0], c[1]]) << (8 - precision)) as u8)
                    .collect();
                (Pixels::U8(expand_gray(&samples)), 8)
            }
        }
        PixelFormat::L16 => {
            let precision = lossless_sof_params(data).map(|(p, _)| p).unwrap_or(16);
            let shift = 16 - precision.min(16);
            let samples: Vec<u16> = pixels
                .as_chunks::<2>().0.iter()
                .map(|c| u16::from_ne_bytes([c[0], c[1]]) << shift)
                .collect();
            (Pixels::U16(expand_gray(&samples)), 16)
        }
    };

    let seg = scan_segments(data);
    let orientation = Orientation::from_exif(seg.exif.as_deref()).unwrap_or_default();

    let frame = ImageFrame {
        width: info.width as u32,
        height: info.height as u32,
        bit_depth,
        has_alpha: false,
        pixels,
    };
    Ok(Decoded::Image(Box::new(
        DecodedImage::new(
            frame,
            ColorDesc::new(None, seg.icc),
            MetaBoxes::new(seg.exif, seg.xmp),
        )
        .orientation(orientation),
    )))
}

/// Grayscale → RGB; the encode path is RGB(A)-only.
fn expand_gray<T: Copy>(samples: &[T]) -> Vec<T> {
    let mut out = Vec::with_capacity(samples.len() * 3);
    for s in samples {
        out.extend_from_slice([*s, *s, *s].as_slice());
    }
    out
}
