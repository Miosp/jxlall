//! JPEG pixel decode via zune-jpeg: the forced-lossy path and inputs jbrd
//! cannot represent (CMYK, arithmetic).

use crate::domain::{ColorDesc, Decoded, DecodedImage, ImageFrame, MetaBoxes, Orientation, Pixels};
use super::jpeg_markers::{scan_segments, MAX_PIXELS};

pub fn decode(data: &[u8]) -> Result<Decoded, String> {
    let mut dec = zune_jpeg::JpegDecoder::new(zune_jpeg::zune_core::bytestream::ZCursor::new(data));
    dec.decode_headers()
        .map_err(|e| format!("JPEG header: {e}"))?;
    let info = dec.info().ok_or("JPEG: no info")?;
    if info.width == 0 || info.height == 0 {
        return Err("JPEG: zero dimension".into());
    }
    if u64::from(info.width) * u64::from(info.height) > MAX_PIXELS {
        return Err(format!("JPEG too large ({}x{})", info.width, info.height));
    }
    let pixels = dec.decode().map_err(|e| format!("JPEG decode: {e}"))?;

    // zune converts CMYK/grayscale to RGB; alpha only when 4 samples/pixel.
    let count = u32::from(info.width) as usize * u32::from(info.height) as usize;
    let has_alpha = pixels.len() == count * 4;

    let seg = scan_segments(data);
    let exif = seg.exif.or(info.exif_data);
    let orientation = Orientation::from_exif(exif.as_deref()).unwrap_or_default();

    let frame = ImageFrame {
        width: u32::from(info.width),
        height: u32::from(info.height),
        bit_depth: 8,
        has_alpha,
        alpha_premultiplied: false,
        pixels: Pixels::U8(pixels),
    };
    Ok(Decoded::Image(Box::new(
        DecodedImage::new(frame, ColorDesc::new(None, seg.icc), MetaBoxes::new(exif, seg.xmp))
            .orientation(orientation),
    )))
}
