//! HEIF/HEIC/AVIF decode via `heic` (std + av1 features).

use crate::domain::{
    reset_exif_orientation, strip_exif_prefix, Cicp, ColorDesc, Decoded, DecodedImage, GainMapData, Hdr, ImageFrame, MetaBoxes, Orientation,
    Pixels,
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
        let px = if info.has_alpha {
            frame.to_rgba16().map_err(|e| format!("to_rgba16: {e}"))?
        } else {
            frame.to_rgb16().map_err(|e| format!("to_rgb16: {e}"))?
        };
        (Pixels::U16(px), 16, w, h)
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
            eprintln!("note: gain map decode failed ({e}), converting base image without it");
            return Ok(None);
        }
    };
    if !gain_map_aligned(source_orientation, (w, h), (g.width, g.height)) {
        eprintln!("note: gain map orientation differs from the base image, dropped");
        return Ok(None);
    }
    match crate::domain::from_heic(g) {
        Ok(g) => Ok(g),
        Err(e) => {
            eprintln!("note: gain map dropped ({e}), converting base image without it");
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
