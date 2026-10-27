//! JPEG marker-stream scans: lossless SOF detection + APP-segment metadata.

use super::xmp_extended;

/// Lossless Huffman SOF code (SOF3). The arithmetic lossless codes
/// SOF11/SOF15 are absent: no pure-Rust decoder handles arithmetic coding.
const SOF_LOSSLESS: u8 = 0xC3;
/// Define Hierarchical Progression: opens every hierarchical JPEG.
const DHP: u8 = 0xDE;
/// Differential (hierarchical-only) SOF codes, Huffman and arithmetic.
const DIFFERENTIAL_SOF: [u8; 6] = [0xC5, 0xC6, 0xC7, 0xCD, 0xCE, 0xCF];
/// C4 (DHT), C8 (JPG extension) and CC (DAC) share the SOF range.
const NOT_SOF: [u8; 3] = [0xC4, 0xC8, 0xCC];

/// Pixel budget for the pixel-decode paths (matches the TIFF page budget):
/// bounds the upfront w*h output allocation in both JPEG decoders.
pub const MAX_PIXELS: u64 = 1 << 28;

/// First frame-defining segment before any scan: a SOFn or DHP marker and
/// its payload (after the length field).
fn first_frame(data: &[u8]) -> Option<(u8, &[u8])> {
    let mut i = 2usize; // past SOI
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            return None; // desynchronized marker stream
        }
        if data[i + 1] == 0xFF {
            i += 1; // fill byte (B.1.1.2): any number may precede a marker
            continue;
        }
        let marker = data[i + 1];
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            return None; // entropy data or EOI before any SOF
        }
        let seg_len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if seg_len < 2 || i + 2 + seg_len > data.len() {
            return None;
        }
        if marker == DHP || ((0xC0..=0xCF).contains(&marker) && !NOT_SOF.contains(&marker)) {
            return Some((marker, &data[i + 4..i + 2 + seg_len]));
        }
        i += 2 + seg_len;
    }
    None
}

pub fn is_lossless_sof(data: &[u8]) -> bool {
    lossless_sof_params(data).is_some()
}

/// Hierarchical JPEGs decode to their first (often reduced) frame only, in
/// every decoder available here: converting one would silently lose detail.
pub fn is_hierarchical(data: &[u8]) -> bool {
    first_frame(data).is_some_and(|(m, _)| m == DHP || DIFFERENTIAL_SOF.contains(&m))
}

/// `(precision_bits, component_count)` from the lossless SOF, if present.
pub fn lossless_sof_params(data: &[u8]) -> Option<(u8, u8)> {
    match first_frame(data)? {
        // SOF payload: precision(1) height(2) width(2) comps(1).
        (SOF_LOSSLESS, body) if body.len() >= 6 => Some((body[0], body[5])),
        _ => None,
    }
}

#[derive(Default)]
pub struct SegmentMeta {
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
    pub icc: Option<Vec<u8>>,
}

/// Walk APP1/APP2 segments: EXIF (`Exif\0\0`), XMP
/// (`http://ns.adobe.com/xap/1.0/\0`), and multi-part APP2
/// (`ICC_PROFILE\0` + sequence counters).
pub fn scan_segments(data: &[u8]) -> SegmentMeta {
    let mut meta = SegmentMeta::default();
    let mut icc_parts: Vec<(u8, &[u8])> = Vec::new();
    let mut icc_total = 0u8;
    let mut xmp_chunks = Vec::new();
    let mut i = 2usize;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            break;
        }
        if data[i + 1] == 0xFF {
            i += 1; // fill byte
            continue;
        }
        let marker = data[i + 1];
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let seg_len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if seg_len < 2 || i + 2 + seg_len > data.len() {
            break;
        }
        let body = &data[i + 4..i + 2 + seg_len];
        if marker == 0xE1 && body.starts_with(b"Exif\0\0") && meta.exif.is_none() {
            meta.exif = Some(body[6..].to_vec());
        } else if marker == 0xE1
            && body.starts_with(b"http://ns.adobe.com/xap/1.0/\0")
            && meta.xmp.is_none()
        {
            meta.xmp = Some(body[29..].to_vec());
        } else if marker == 0xE1
            && let Some(chunk) = xmp_extended::parse_chunk(body)
        {
            xmp_chunks.push(chunk);
        } else if marker == 0xE2 && body.len() > 14 && body.starts_with(b"ICC_PROFILE\0") {
            let seq = body[12];
            icc_total = icc_total.max(body[13]);
            if seq > 0 {
                icc_parts.push((seq, &body[14..]));
            }
        }
        i += 2 + seg_len;
    }
    // Only trust a complete ICC chunk set: sequence numbers must cover
    // 1..=total exactly — no missing parts, no duplicates.
    if !icc_parts.is_empty() && icc_total > 0 && icc_parts.len() == icc_total as usize && {
        icc_parts.sort_by_key(|(seq, _)| *seq);
        icc_parts.iter().map(|(seq, _)| *seq).eq(1..=icc_total)
    } {
        let mut icc = Vec::new();
        for (_, part) in &icc_parts {
            icc.extend_from_slice(part);
        }
        meta.icc = Some(icc);
    }
    if let Some(main) = meta.xmp.as_deref()
        && !xmp_chunks.is_empty()
    {
        match xmp_extended::merge(main, &xmp_chunks) {
            Ok(merged) => meta.xmp = Some(merged),
            Err(e) => eprintln!("note: extended XMP dropped ({e}), main packet kept"),
        }
    }
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SOI, then `FF FF` fill before an APP1 EXIF segment and a SOF3.
    fn jpeg_with_fill_bytes() -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF];
        let exif = b"Exif\0\0MM\0*";
        v.extend_from_slice(&[0xFF, 0xE1]);
        v.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        v.extend_from_slice(exif);
        v.extend_from_slice(&[0xFF, 0xFF, 0xC3, 0x00, 0x0B, 12, 0, 1, 0, 1, 1, 1, 0x11, 0]);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    #[test]
    fn hierarchical_is_detected_before_first_frame() {
        // SOI, DHP(64x64), SOF0(32x32).
        let mut v = vec![
            0xFF, 0xD8, 0xFF, DHP, 0x00, 0x0B, 8, 0, 64, 0, 64, 1, 1, 0x11, 0,
        ];
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 8, 0, 32, 0, 32, 1, 1, 0x11, 0]);
        assert!(is_hierarchical(&v));
        assert!(!is_lossless_sof(&v));
        assert!(!is_hierarchical(&jpeg_with_fill_bytes()));
    }

    fn app1(v: &mut Vec<u8>, body: &[u8]) {
        v.extend_from_slice(&[0xFF, 0xE1]);
        v.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        v.extend_from_slice(body);
    }

    #[test]
    fn extended_xmp_is_merged_into_main_packet() {
        use xmp_extended::tests::{chunk_bodies, extended_packet, main_packet};
        let mut v = vec![0xFF, 0xD8];
        let mut main = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        main.extend_from_slice(main_packet().as_bytes());
        app1(&mut v, &main);
        for body in chunk_bodies(extended_packet().as_bytes()) {
            app1(&mut v, &body);
        }
        v.extend_from_slice(&[0xFF, 0xD9]);
        let xmp = String::from_utf8(scan_segments(&v).xmp.unwrap()).unwrap();
        assert!(xmp.contains(r#"GDepth:Data="AAAA""#));
        assert!(!xmp.contains("HasExtendedXMP"));
    }

    #[test]
    fn fill_bytes_do_not_hide_segments() {
        let data = jpeg_with_fill_bytes();
        assert_eq!(scan_segments(&data).exif.as_deref(), Some(&b"MM\0*"[..]));
        assert_eq!(lossless_sof_params(&data), Some((12, 1)));
    }
}
