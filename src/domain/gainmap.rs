//! Gain maps: HEIF/AVIF ISO 21496-1 → JXL `jhgm` box.
//!
//! Sources:
//! - AVIF `tmap` derived item: `heic::HdrGainMap::iso21496` (version byte
//!   prefix, `AvifTmap` layout).
//! - Apple HEIC auxiliary item: `heic::HdrGainMap::xmp`
//!   (`urn:com:apple:photo:2020:aux:hdrgainmap`), converted field-by-field.
//!
//! Target: `jhgm` = JxlGainMapBundle{ metadata (ISO 21496-1, `JpegApp2`
//! layout = no version prefix), gain map as naked JXL codestream }.

use crate::encode::{build_jhgm_box, encode_gain_map_gray, EncodeError, EncodeSettings};
use ultrahdr_core::metadata::iso21496::{parse_iso21496, serialize_iso21496};
use ultrahdr_core::{GainMapMetadata, Iso21496Format};

/// Gain map ready for `jhgm` assembly.
pub struct GainMapData {
    iso_metadata: Vec<u8>,
    width: u32,
    height: u32,
    gray: Vec<u8>,
}

impl GainMapData {
    /// Serialize the `jhgm` box payload.
    pub fn to_jhgm(&self, settings: &EncodeSettings) -> Result<Vec<u8>, EncodeError> {
        let codestream = encode_gain_map_gray(self.width, self.height, &self.gray, settings)?;
        build_jhgm_box(&self.iso_metadata, &codestream)
    }
}

/// Convert a decoded heic gain map; `None` when the file carries neither the
/// tmap binary metadata nor the Apple XMP. Consumes `g` — the pixel buffer
/// moves in, no copy.
pub fn from_heic(g: heic::HdrGainMap) -> Result<Option<GainMapData>, String> {
    if let Some(tmap) = g.iso21496.as_deref() {
        // AvifTmap carries a version byte; the jhgm bundle wants the bare
        // ISO 21496-1 payload.
        let md = parse_iso21496(tmap, Iso21496Format::AvifTmap)
            .map_err(|e| format!("tmap metadata: {e}"))?;
        let iso = serialize_iso21496(&md, Iso21496Format::JpegApp2);
        return Ok(Some(GainMapData {
            iso_metadata: iso,
            width: g.width,
            height: g.height,
            gray: g.data,
        }));
    }
    if let Some(xmp) = g.xmp.as_deref() {
        let xmp = std::str::from_utf8(xmp).map_err(|e| format!("gain map xmp: {e}"))?;
        let md = apple_xmp_to_metadata(xmp)?;
        let iso = serialize_iso21496(&md, Iso21496Format::JpegApp2);
        return Ok(Some(GainMapData {
            iso_metadata: iso,
            width: g.width,
            height: g.height,
            gray: g.data,
        }));
    }
    Ok(None)
}

/// Apple `urn:com:apple:photo:2020:aux:hdrgainmap` XMP → ISO 21496-1.
///
/// Field mapping (matches libultrahdr's apple→ISO conversion):
/// - HDRCapacityMin/Max (log2 stops) → base/alternate_hdr_headroom
/// - GainMapMin/Max (log2 gain) → gain_map_min/max
/// - Gamma → gamma; OffsetSDR → base_offset; OffsetHDR → alternate_offset
///
/// Apple writes rational fields as `auxi:<Name>Numerator`/`<Name>Denominator`
/// pairs; HDRCapacity* are plain decimals.
fn apple_xmp_to_metadata(xmp: &str) -> Result<GainMapMetadata, String> {
    let mut md = GainMapMetadata::new();
    md.gain_map_min = [ratio_or(xmp, "GainMapMin", 0.0); 3];
    md.gain_map_max = [ratio_or(xmp, "GainMapMax", 1.0); 3];
    md.gamma = [ratio_or(xmp, "Gamma", 1.0); 3];
    md.base_offset = [ratio_or(xmp, "OffsetSDR", 1.0 / 64.0); 3];
    md.alternate_offset = [ratio_or(xmp, "OffsetHDR", 1.0 / 64.0); 3];
    md.base_hdr_headroom = attr_f64(xmp, "HDRCapacityMin").unwrap_or(0.0);
    md.alternate_hdr_headroom = attr_f64(xmp, "HDRCapacityMax").unwrap_or(1.0);
    md.use_base_color_space = true;
    md.backward_direction = false;
    if ![md.base_hdr_headroom, md.alternate_hdr_headroom]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err("apple gain map: non-finite value".into());
    }
    for arr in [
        &mut md.gain_map_min,
        &mut md.gain_map_max,
        &mut md.gamma,
        &mut md.base_offset,
        &mut md.alternate_offset,
    ] {
        for v in arr.iter_mut() {
            if !v.is_finite() {
                return Err("apple gain map: non-finite value".into());
            }
        }
    }
    Ok(md)
}

fn attr_f64(xmp: &str, name: &str) -> Option<f64> {
    extract_attr(xmp, name).and_then(|v| v.parse().ok())
}

/// Apple rationals: `<name>Numerator`/`<name>Denominator`, else plain attr.
fn ratio_or(xmp: &str, name: &str, default: f64) -> f64 {
    let num = extract_attr(xmp, &format!("{name}Numerator")).and_then(|v| v.parse::<f64>().ok());
    let den = extract_attr(xmp, &format!("{name}Denominator")).and_then(|v| v.parse::<f64>().ok());
    match (num, den) {
        (Some(n), Some(d)) if d != 0.0 => n / d,
        _ => attr_f64(xmp, name).unwrap_or(default),
    }
}

/// Find `name="value"` scanning both `auxi:name` and bare `name`.
fn extract_attr(xmp: &str, name: &str) -> Option<String> {
    let auxi = format!("auxi:{name}=\"");
    let bare = format!("{name}=\"");
    for prefix in [auxi.as_str(), bare.as_str()] {
        let Some(start) = xmp.find(prefix) else { continue };
        let rest = &xmp[start + prefix.len()..];
        let Some(end) = rest.find('"') else { continue };
        return Some(rest[..end].to_string());
    }
    None
}
