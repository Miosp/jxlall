//! JPEG marker-stream scans: lossless SOF detection + APP-segment metadata.

/// Lossless (and differential) Huffman SOF codes: SOF3, SOF5, SOF7.
/// SOF9/SOF11 are arithmetic LOSSY; SOF13/SOF15 are arithmetic lossless —
/// no pure-Rust decoder handles arithmetic coding, so they are not here.
const LOSSLESS_SOF: [u8; 3] = [0xC3, 0xC5, 0xC7];

/// Pixel budget for the pixel-decode paths (matches the TIFF page budget):
/// bounds the upfront w*h output allocation in both JPEG decoders.
pub const MAX_PIXELS: u64 = 1 << 28;

pub fn is_lossless_sof(data: &[u8]) -> bool {
    lossless_sof_params(data).is_some()
}

/// `(precision_bits, component_count)` from the lossless SOF, if present.
pub fn lossless_sof_params(data: &[u8]) -> Option<(u8, u8)> {
    let mut i = 2usize; // past SOI
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            return None; // desynchronized marker stream
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
        if LOSSLESS_SOF.contains(&marker) {
            // SOF payload: len(2) precision(1) height(2) width(2) comps(1).
            if seg_len < 8 {
                return None; // truncated SOF header
            }
            return Some((data[i + 4], data[i + 9]));
        }
        i += 2 + seg_len;
    }
    None
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
    let mut i = 2usize;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            break;
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
    if !icc_parts.is_empty()
        && icc_total > 0
        && icc_parts.len() == icc_total as usize
        && {
            icc_parts.sort_by_key(|(seq, _)| *seq);
            icc_parts.iter().map(|(seq, _)| *seq).eq(1..=icc_total)
        }
    {
        let mut icc = Vec::new();
        for (_, part) in &icc_parts {
            icc.extend_from_slice(part);
        }
        meta.icc = Some(icc);
    }
    meta
}
