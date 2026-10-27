//! JXL metadata boxes assembled from source containers.

use std::sync::Arc;

/// JPEG APP1 EXIF signature that wild PNG/WebP writers leave in front of the
/// TIFF header.
const EXIF_APP1_PREFIX: &[u8] = b"Exif\0\0";

/// Bare TIFF payload: drops a leading `Exif\0\0` signature if present.
pub fn strip_exif_prefix(mut exif: Vec<u8>) -> Vec<u8> {
    if exif.starts_with(EXIF_APP1_PREFIX) {
        exif.drain(..EXIF_APP1_PREFIX.len());
    }
    exif
}

/// TIFF tag holding the EXIF orientation.
const ORIENTATION_TAG: u16 = 274;
/// TIFF SHORT field type.
const TYPE_SHORT: u16 = 3;
const IFD_ENTRY_LEN: usize = 12;

/// Set IFD0's Orientation to 1 in place (port of libjxl
/// `ResetExifOrientation`). For sources whose pixels already carry the
/// rotation, a stale tag would make Exif-honoring readers rotate again.
pub fn reset_exif_orientation(exif: &mut [u8]) {
    let big_endian = match exif.get(..4) {
        Some(b"MM\0*") => true,
        Some(b"II*\0") => false,
        _ => return,
    };
    let u16_at = |b: &[u8], i: usize| {
        let v = [b[i], b[i + 1]];
        if big_endian { u16::from_be_bytes(v) } else { u16::from_le_bytes(v) }
    };
    let Some(ifd) = exif.get(4..8).map(|b| {
        let v = [b[0], b[1], b[2], b[3]];
        (if big_endian { u32::from_be_bytes(v) } else { u32::from_le_bytes(v) }) as usize
    }) else {
        return;
    };
    if ifd < 8 || ifd.saturating_add(2) > exif.len() {
        return;
    }
    let count = usize::from(u16_at(exif, ifd));
    for n in 0..count {
        let e = ifd + 2 + n * IFD_ENTRY_LEN;
        if e + IFD_ENTRY_LEN > exif.len() {
            return;
        }
        if u16_at(exif, e) == ORIENTATION_TAG {
            let value_count = &exif[e + 4..e + 8];
            let single = if big_endian { value_count == [0, 0, 0, 1] } else { value_count == [1, 0, 0, 0] };
            if u16_at(exif, e + 2) == TYPE_SHORT && single {
                let one = if big_endian { 1u16.to_be_bytes() } else { 1u16.to_le_bytes() };
                exif[e + 8..e + 10].copy_from_slice(&one);
            }
            return;
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct MetaBoxes {
    /// Raw EXIF payload (TIFF structure, no 4-byte offset prefix).
    /// `Arc`: TIFF passes the whole file, shared with the decoder input.
    pub exif: Option<Arc<Vec<u8>>>,
    pub xmp: Option<Arc<Vec<u8>>>,
    /// Assembled `jhgm` gain map payload (JxlGainMapBundle bytes).
    pub jhgm: Option<Vec<u8>>,
}

impl MetaBoxes {
    /// Takes owned payloads; the `Vec`s move into their `Arc`, no byte copy.
    pub fn new(exif: Option<Vec<u8>>, xmp: Option<Vec<u8>>) -> Self {
        Self {
            exif: exif.map(Arc::new),
            xmp: xmp.map(Arc::new),
            jhgm: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.exif.is_none() && self.xmp.is_none() && self.jhgm.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Big- and little-endian TIFF with IFD0 = { Orientation SHORT 1 = 6 }.
    fn exif_with_orientation(big_endian: bool) -> Vec<u8> {
        let w16 = |v: u16| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
        let w32 = |v: u32| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
        let mut v = if big_endian { b"MM\0*".to_vec() } else { b"II*\0".to_vec() };
        v.extend_from_slice(&w32(8));
        v.extend_from_slice(&w16(1));
        v.extend_from_slice(&w16(ORIENTATION_TAG));
        v.extend_from_slice(&w16(TYPE_SHORT));
        v.extend_from_slice(&w32(1));
        v.extend_from_slice(&w16(6));
        v.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        v
    }

    #[test]
    fn resets_orientation_both_byte_orders() {
        for be in [true, false] {
            let mut exif = exif_with_orientation(be);
            reset_exif_orientation(&mut exif);
            let o = crate::domain::Orientation::from_exif(Some(&exif));
            assert_eq!(o.map(|o| o.get()), Some(1), "big_endian={be}");
        }
    }

    #[test]
    fn reset_ignores_truncated_input() {
        let mut exif = exif_with_orientation(false);
        exif.truncate(14);
        reset_exif_orientation(&mut exif);
        reset_exif_orientation(&mut []);
    }

    #[test]
    fn strips_app1_signature_only_when_present() {
        assert_eq!(strip_exif_prefix(b"Exif\0\0MM\0*".to_vec()), b"MM\0*");
        assert_eq!(strip_exif_prefix(b"II*\0".to_vec()), b"II*\0");
    }
}
