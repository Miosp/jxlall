//! WebP decode via `zenwebp`: 8-bit RGB(A) + ICC/EXIF/XMP metadata.

use crate::domain::{
    ColorDesc, Decoded, DecodedImage, ImageFrame, MetaBoxes, Orientation, Pixels,
};

pub fn decode(data: &[u8]) -> Result<Decoded, String> {
    let mut info = zenwebp::ImageInfo::from_webp(data).map_err(|e| format!("WebP probe: {e}"))?;
    if info.has_animation {
        return Err("animated WebP not supported (stills only)".into());
    }

    let (px, w, h) = if info.has_alpha {
        zenwebp::oneshot::decode_rgba(data).map_err(|e| format!("WebP decode: {e}"))?
    } else {
        zenwebp::oneshot::decode_rgb(data).map_err(|e| format!("WebP decode: {e}"))?
    };

    // zenwebp parses EXIF orientation itself; fall back to the EXIF payload.
    let orientation = info
        .orientation
        .map(|o| Orientation::new(o as u8))
        .or_else(|| Orientation::from_exif(info.exif.as_deref()))
        .unwrap_or_default();

    let frame = ImageFrame {
        width: w,
        height: h,
        bit_depth: 8,
        has_alpha: info.has_alpha,
        pixels: Pixels::U8(px),
    };
    // WebP has no CICP; ICC only.
    Ok(Decoded::Image(Box::new(
        DecodedImage::new(
            frame,
            ColorDesc::new(None, std::mem::take(&mut info.icc_profile)),
            MetaBoxes::new(std::mem::take(&mut info.exif), std::mem::take(&mut info.xmp)),
        )
        .orientation(orientation),
    )))
}
