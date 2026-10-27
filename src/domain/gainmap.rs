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

/// Convert a decoded heic gain map; `None` when the file carries neither
/// usable tmap binary metadata nor recognizable Apple XMP. Consumes `g` —
/// the pixel buffer moves in, no copy.
pub fn from_heic(g: heic::HdrGainMap) -> Result<Option<GainMapData>, String> {
    let iso_metadata = if let Some(tmap) = g.iso21496.as_deref() {
        tmap_metadata(tmap)?
    } else if let Some(xmp) = g.xmp.as_deref() {
        let xmp = std::str::from_utf8(xmp).map_err(|e| format!("gain map xmp: {e}"))?;
        let Some(md) = apple_xmp_to_metadata(xmp)? else {
            return Err("Apple gain map XMP has no ISO 21496-1 fields".into());
        };
        serialize_iso21496(&md, Iso21496Format::JpegApp2)
    } else {
        return Ok(None);
    };
    Ok(Some(GainMapData {
        iso_metadata,
        width: g.width,
        height: g.height,
        gray: g.data,
    }))
}

/// Validated ISO 21496-1 payload of an AVIF `tmap` item. Past the version
/// byte it is exactly what the jhgm bundle wants, so it is copied verbatim
/// rather than re-serialized (which would re-quantize the rationals).
fn tmap_metadata(tmap: &[u8]) -> Result<Vec<u8>, String> {
    let md = parse_iso21496(tmap, Iso21496Format::AvifTmap)
        .map_err(|e| format!("tmap metadata: {e}"))?;
    // heic exposes only the Y plane: per-channel metadata would be paired
    // with the wrong image.
    let multichannel_flag = tmap
        .get(TMAP_FLAGS_OFFSET)
        .is_some_and(|f| f & TMAP_FLAG_MULTI_CHANNEL != 0);
    if multichannel_flag || !is_single_channel(&md) {
        return Err("multichannel gain map metadata, only one plane available".into());
    }
    Ok(tmap[TMAP_VERSION_LEN..].to_vec())
}

/// AvifTmap prefixes the ISO 21496-1 payload with a one-byte version.
const TMAP_VERSION_LEN: usize = 1;
/// Flags byte: after version(1), minimum_version(2), writer_version(2).
const TMAP_FLAGS_OFFSET: usize = 5;
const TMAP_FLAG_MULTI_CHANNEL: u8 = 0x80;

fn is_single_channel(md: &GainMapMetadata) -> bool {
    let same = |a: &[f64; 3]| a[0] == a[1] && a[1] == a[2];
    same(&md.gain_map_min)
        && same(&md.gain_map_max)
        && same(&md.gamma)
        && same(&md.base_offset)
        && same(&md.alternate_offset)
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
///
/// `None` when none of the fields is present: defaults alone would describe
/// a gain map that does not exist.
fn apple_xmp_to_metadata(xmp: &str) -> Result<Option<GainMapMetadata>, String> {
    const FIELDS: [&str; 7] = [
        "GainMapMin",
        "GainMapMax",
        "Gamma",
        "OffsetSDR",
        "OffsetHDR",
        "HDRCapacityMin",
        "HDRCapacityMax",
    ];
    let present = |f: &str| {
        extract_attr(xmp, f).is_some() || extract_attr(xmp, &format!("{f}Numerator")).is_some()
    };
    if !FIELDS.iter().any(|f| present(f)) {
        return Ok(None);
    }
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
    Ok(Some(md))
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
        // The match must start a whole (local) attribute name, so `Gamma`
        // does not hit `GainMapGamma`; any namespace prefix is accepted.
        let Some((start, _)) = xmp
            .match_indices(prefix)
            .find(|&(i, _)| !xmp[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_'))
        else {
            continue;
        };
        let rest = &xmp[start + prefix.len()..];
        let Some(end) = rest.find('"') else { continue };
        return Some(rest[..end].to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_xmp_without_fields_yields_none() {
        let xmp = r#"<rdf:Description HDRGainMap:HDRGainMapVersion="65536"/>"#;
        assert!(apple_xmp_to_metadata(xmp).unwrap().is_none());
    }

    #[test]
    fn apple_xmp_with_headroom_is_parsed() {
        let xmp = r#"<rdf:Description auxi:HDRCapacityMax="2.5"/>"#;
        let md = apple_xmp_to_metadata(xmp).unwrap().expect("parsed");
        assert_eq!(md.alternate_hdr_headroom, 2.5);
    }

    #[test]
    fn attribute_names_are_matched_whole() {
        let xmp = r#"<x auxi:GainMapGamma="8" hdrgm:Gamma="2"/>"#;
        assert_eq!(extract_attr(xmp, "Gamma").as_deref(), Some("2"));
    }

    #[test]
    fn tmap_payload_is_copied_verbatim() {
        let mut md = GainMapMetadata::new();
        md.gain_map_max = [1.3; 3];
        md.alternate_hdr_headroom = 1.7;
        let tmap = serialize_iso21496(&md, Iso21496Format::AvifTmap);
        assert_eq!(tmap_metadata(&tmap).unwrap(), tmap[TMAP_VERSION_LEN..]);
    }

    #[test]
    fn multichannel_tmap_is_rejected() {
        let mut md = GainMapMetadata::new();
        md.gain_map_max = [1.0, 2.0, 3.0];
        let tmap = serialize_iso21496(&md, Iso21496Format::AvifTmap);
        assert!(tmap_metadata(&tmap).is_err());
        // Equal channels but the multichannel flag set: still 3 declared.
        md.gain_map_max = [2.0; 3];
        let mut tmap = serialize_iso21496(&md, Iso21496Format::AvifTmap);
        tmap[TMAP_FLAGS_OFFSET] |= TMAP_FLAG_MULTI_CHANNEL;
        assert!(tmap_metadata(&tmap).is_err());
    }
}
