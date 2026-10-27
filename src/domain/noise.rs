//! Photon-noise ISO from EXIF. libjxl's grain model assumes a full-frame
//! (36x24 mm) sensor, so the recorded ISO is converted to its full-frame
//! equivalent via the crop factor when the focal lengths are known.

/// `PhotographicSensitivity` is a SHORT; cameras write this when the real
/// value does not fit and put it in the other sensitivity tags instead.
const SATURATED_ISO: u32 = 65535;
/// Crop factors outside this range (medium format ~0.6 to small phone
/// sensors ~7) are treated as bogus focal-length tags.
const CROP_RANGE: std::ops::RangeInclusive<f64> = 0.5..=10.0;

/// Sensitivity found in the metadata.
#[derive(Debug, PartialEq)]
pub struct IsoEstimate {
    pub iso: f64,
    /// `FocalLengthIn35mmFilm / FocalLength`, when both are usable.
    pub crop: Option<f64>,
}

impl IsoEstimate {
    /// Full-frame-equivalent ISO: noise per pixel area scales with crop².
    pub fn full_frame_iso(&self) -> f64 {
        self.iso * self.crop.map_or(1.0, |c| c * c)
    }
}

/// Read ISO and crop factor from a bare TIFF/EXIF payload.
pub fn iso_from_exif(exif: &[u8]) -> Option<IsoEstimate> {
    let exif = exif::Reader::new().read_raw(exif.to_vec()).ok()?;
    let uint = |tag| {
        exif.get_field(tag, exif::In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
            .filter(|&v| v > 0)
    };
    let sensitivity = uint(exif::Tag::PhotographicSensitivity);
    // A saturated value is still a lower bound when nothing better exists.
    let iso = sensitivity
        .filter(|&v| v < SATURATED_ISO)
        .or_else(|| uint(exif::Tag::RecommendedExposureIndex))
        .or_else(|| uint(exif::Tag::ISOSpeed))
        .or(sensitivity)?;
    let focal = exif
        .get_field(exif::Tag::FocalLength, exif::In::PRIMARY)
        .and_then(|f| match &f.value {
            exif::Value::Rational(v) => v.first().map(exif::Rational::to_f64),
            _ => None,
        })
        .filter(|f| f.is_finite() && *f > 0.0);
    let crop = focal
        .zip(uint(exif::Tag::FocalLengthIn35mmFilm))
        .map(|(f, f35)| f64::from(f35) / f)
        .filter(|c| CROP_RANGE.contains(c));
    Some(IsoEstimate { iso: f64::from(iso), crop })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXIF_IFD_POINTER: u16 = 0x8769;
    const TYPE_SHORT: u16 = 3;
    const TYPE_LONG: u16 = 4;
    const TYPE_RATIONAL: u16 = 5;

    /// Little-endian TIFF: IFD0 -> Exif IFD holding `tags` (SHORT/LONG
    /// inline values, plus an optional FocalLength rational).
    fn exif(tags: &[(u16, u16, u32)], focal: Option<(u32, u32)>) -> Vec<u8> {
        let mut entries: Vec<(u16, u16, u32)> = tags.to_vec();
        let n = entries.len() + usize::from(focal.is_some());
        let exif_ifd = 8 + 2 + 12 + 4;
        let rational_at = exif_ifd + 2 + n * 12 + 4;
        if focal.is_some() {
            entries.push((0x920a, TYPE_RATIONAL, rational_at as u32));
        }
        entries.sort_by_key(|e| e.0);
        let mut v = b"II*\0".to_vec();
        v.extend_from_slice(&8u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&EXIF_IFD_POINTER.to_le_bytes());
        v.extend_from_slice(&TYPE_LONG.to_le_bytes());
        v.extend_from_slice(&1u32.to_le_bytes());
        v.extend_from_slice(&(exif_ifd as u32).to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&(n as u16).to_le_bytes());
        for (tag, typ, value) in entries {
            v.extend_from_slice(&tag.to_le_bytes());
            v.extend_from_slice(&typ.to_le_bytes());
            v.extend_from_slice(&1u32.to_le_bytes());
            v.extend_from_slice(&value.to_le_bytes());
        }
        v.extend_from_slice(&0u32.to_le_bytes());
        if let Some((num, den)) = focal {
            v.extend_from_slice(&num.to_le_bytes());
            v.extend_from_slice(&den.to_le_bytes());
        }
        v
    }

    #[test]
    fn phone_iso_scaled_by_crop_squared() {
        // 6.86 mm lens reported as 24 mm equivalent: crop ~3.5.
        let e = iso_from_exif(&exif(
            &[(0x8827, TYPE_SHORT, 400), (0xa405, TYPE_SHORT, 24)],
            Some((686, 100)),
        ))
        .unwrap();
        assert_eq!(e.iso, 400.0);
        let crop = 24.0 / 6.86;
        assert!((e.full_frame_iso() - 400.0 * crop * crop).abs() < 1e-6);
    }

    #[test]
    fn saturated_iso_falls_back_to_other_tags() {
        let e = iso_from_exif(&exif(
            &[(0x8827, TYPE_SHORT, 65535), (0x8832, TYPE_LONG, 102_400)],
            None,
        ))
        .unwrap();
        assert_eq!(e, IsoEstimate { iso: 102_400.0, crop: None });
    }

    #[test]
    fn saturated_iso_alone_is_a_lower_bound() {
        let e = iso_from_exif(&exif(&[(0x8827, TYPE_SHORT, 65535)], None)).unwrap();
        assert_eq!(e.iso, 65535.0);
    }

    #[test]
    fn implausible_crop_is_ignored() {
        let e = iso_from_exif(&exif(
            &[(0x8827, TYPE_SHORT, 100), (0xa405, TYPE_SHORT, 50)],
            Some((500, 1)),
        ))
        .unwrap();
        assert_eq!(e.crop, None);
        assert_eq!(e.full_frame_iso(), 100.0);
    }

    #[test]
    fn missing_iso_is_none() {
        assert!(iso_from_exif(&exif(&[(0xa405, TYPE_SHORT, 24)], None)).is_none());
        assert!(iso_from_exif(b"garbage").is_none());
    }
}
