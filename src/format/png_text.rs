//! PNG textual-chunk metadata the png crate does not surface: XMP in iTXt
//! (`XML:com.adobe.xmp`) and ImageMagick hex-encoded raw profiles in tEXt.

#[derive(Default)]
pub struct TextChunks {
    pub xmp: Option<Vec<u8>>,
    /// EXIF from ImageMagick's `Raw profile type exif` tEXt chunks.
    pub raw_exif: Option<Vec<u8>>,
}

pub fn scan_text_chunks(data: &[u8]) -> TextChunks {
    let mut out = TextChunks::default();
    // PNG signature is 8 bytes; each chunk is len(4) type(4) data crc(4).
    let mut i = 8usize;
    while i + 8 <= data.len() {
        let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        let Some(end) = len.checked_add(i + 12).filter(|&end| end <= data.len()) else {
            break;
        };
        let typ = &data[i + 4..i + 8];
        let body = &data[i + 8..i + 8 + len];
        if typ == b"iTXt" && out.xmp.is_none() {
            if let Some(xmp) = parse_itxt_xmp(body) {
                out.xmp = Some(xmp);
            }
        } else if typ == b"tEXt" {
            let (keyword, text) = split_nul(body);
            match keyword {
                b"Raw profile type exif" if out.raw_exif.is_none() => {
                    out.raw_exif = decode_raw_profile(text);
                }
                b"Raw profile type xmp" if out.xmp.is_none() => {
                    out.xmp = decode_raw_profile(text);
                }
                _ => {}
            }
        }
        i = end;
    }
    out
}

/// iTXt body: `keyword\0 compression_flag compression_method language\0
/// translated_keyword\0 text`. XMP is stored uncompressed.
fn parse_itxt_xmp(body: &[u8]) -> Option<Vec<u8>> {
    let (keyword, rest) = split_nul(body);
    if keyword != b"XML:com.adobe.xmp" || rest.len() < 2 {
        return None;
    }
    if rest[0] != 0 {
        return None; // compressed XMP: not expected
    }
    let (_lang, rest) = split_nul(&rest[2..]);
    let (_translated, text) = split_nul(rest);
    (!text.is_empty()).then(|| text.to_vec())
}

fn split_nul(buf: &[u8]) -> (&[u8], &[u8]) {
    match buf.iter().position(|&b| b == 0) {
        Some(p) => (&buf[..p], &buf[p + 1..]),
        None => (buf, &[]),
    }
}

/// ImageMagick raw profile payload: `\n<name>\n<count> [w]\n<hex>\n...`
/// The keyword prefix is already stripped; skip the name and dimension
/// lines, then hex-decode the rest.
fn decode_raw_profile(text: &[u8]) -> Option<Vec<u8>> {
    let mut lines = text.split(|&b| b == b'\n').filter(|l| !l.is_empty());
    let _name = lines.next()?;
    let _dims = lines.next()?; // "<count>" or "<width> <count>"
    let hex: Vec<u8> = lines
        .flat_map(|l| l.iter().copied())
        .filter(|b| b.is_ascii_hexdigit())
        .collect();
    if !hex.len().is_multiple_of(2) || hex.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_chunks::<2>().0 {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}
