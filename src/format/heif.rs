//! HEIF/HEIC/AVIF decode via `heic` (std + av1 features).

use crate::domain::{
    Cicp, ColorDesc, Decoded, DecodedImage, GainMapData, Hdr, ImageFrame, MetaBoxes, Orientation,
    Pixels,
};

pub fn decode(data: &[u8]) -> Result<Decoded, String> {
    let mut info = heic::ImageInfo::from_bytes(data).map_err(|e| format!("HEIF probe: {e}"))?;

    let cicp = (info.color_primaries != 0 || info.transfer_characteristics != 0).then_some(Cicp {
        color_primaries: info.color_primaries,
        transfer_characteristics: info.transfer_characteristics,
    });
    let hdr = Hdr::for_transfer(info.transfer_characteristics);
    let orientation = Orientation::from_exif(info.exif.as_deref()).unwrap_or_default();

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
        let px = if info.has_alpha {
            frame.to_rgba16().map_err(|e| format!("to_rgba16: {e}"))?
        } else {
            frame.to_rgb16().map_err(|e| format!("to_rgb16: {e}"))?
        };
        (Pixels::U16(px), 16, w, h)
    };

    let gain_map = decode_gain_map(data, &info)?;

    let frame = ImageFrame {
        width: w,
        height: h,
        bit_depth,
        has_alpha: info.has_alpha,
        pixels,
    };
    Ok(Decoded::Image(Box::new(
        DecodedImage::new(
            frame,
            ColorDesc::new(cicp, std::mem::take(&mut info.icc_profile)),
            MetaBoxes::new(std::mem::take(&mut info.exif), std::mem::take(&mut info.xmp)),
        )
        .orientation(orientation)
        .hdr(hdr)
        .gain_map(gain_map),
    )))
}

fn decode_gain_map(data: &[u8], info: &heic::ImageInfo) -> Result<Option<GainMapData>, String> {
    if !info.has_gain_map {
        return Ok(None);
    }
    let g = match heic::DecoderConfig::new().decode_gain_map(data) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("note: gain map decode failed ({e}), converting base image without it");
            return Ok(None);
        }
    };
    crate::domain::from_heic(g)
}

