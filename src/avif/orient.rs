//! AVIF output bakes EXIF orientation into the pixels: the encode is lossy
//! anyway, and viewers disagree on whether `irot`/`imir` or Exif wins.

/// Pixels as displayed for EXIF `orientation` (1..=8), interleaved with
/// `ch` channels. Returns the new buffer and its width and height.
pub fn apply<T: Copy>(src: &[T], w: u32, h: u32, ch: usize, orientation: u8) -> (Vec<T>, u32, u32) {
    if orientation <= 1 || orientation > 8 {
        return (src.to_vec(), w, h);
    }
    let (sw, sh) = (w as usize, h as usize);
    let swap = orientation >= 5;
    let (dw, dh) = if swap { (sh, sw) } else { (sw, sh) };
    let mut out = Vec::with_capacity(src.len());
    for dy in 0..dh {
        for dx in 0..dw {
            let (sx, sy) = match orientation {
                2 => (sw - 1 - dx, dy),
                3 => (sw - 1 - dx, sh - 1 - dy),
                4 => (dx, sh - 1 - dy),
                5 => (dy, dx),
                6 => (dy, sh - 1 - dx),
                7 => (sw - 1 - dy, sh - 1 - dx),
                _ => (sw - 1 - dy, dx), // 8
            };
            let i = (sy * sw + sx) * ch;
            out.extend_from_slice(&src[i..i + ch]);
        }
    }
    (out, dw as u32, dh as u32)
}

const XMP_ORIENTATION: &[u8] = b"tiff:Orientation";

/// Set `tiff:Orientation` to 1 in an XMP packet, in place (attribute or
/// element form, single-digit values only, so the length is unchanged).
pub fn reset_xmp_orientation(xmp: &mut [u8]) {
    let mut from = 0;
    while let Some(pos) = find(&xmp[from..], XMP_ORIENTATION).map(|p| p + from) {
        let mut i = pos + XMP_ORIENTATION.len();
        from = i;
        // The whole name: `tiff:Orientation="6"`, `='6'` or `>6<`.
        if !matches!(xmp.get(i), Some(b' ' | b'=' | b'>')) {
            continue;
        }
        while i < xmp.len() && matches!(xmp[i], b' ' | b'=' | b'"' | b'\'' | b'>') {
            i += 1;
        }
        if i + 1 < xmp.len() && (b'2'..=b'8').contains(&xmp[i]) && !xmp[i + 1].is_ascii_digit() {
            xmp[i] = b'1';
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

const EXIF_IFD_POINTER: u16 = 0x8769;
const COLOR_SPACE: u16 = 0xA001;
const COLOR_SPACE_SRGB: u16 = 1;
const COLOR_SPACE_UNCALIBRATED: u16 = 0xFFFF;

/// EXIF `ColorSpace = 1` (sRGB) on wide-gamut output makes Immich render
/// an sRGB (clipped) preview; set it to Uncalibrated, in place.
pub fn mark_exif_uncalibrated(exif: &mut [u8]) {
    let be = match exif.get(..4) {
        Some(b"MM\0*") => true,
        Some(b"II*\0") => false,
        _ => return,
    };
    let u16_at = |b: &[u8], i: usize| -> Option<u16> {
        let v = [*b.get(i)?, *b.get(i + 1)?];
        Some(if be {
            u16::from_be_bytes(v)
        } else {
            u16::from_le_bytes(v)
        })
    };
    let u32_at = |b: &[u8], i: usize| -> Option<usize> {
        let v: [u8; 4] = b.get(i..i + 4)?.try_into().ok()?;
        Some(
            (if be {
                u32::from_be_bytes(v)
            } else {
                u32::from_le_bytes(v)
            }) as usize,
        )
    };
    let find_entry = |b: &[u8], ifd: usize, tag: u16| -> Option<usize> {
        let n = usize::from(u16_at(b, ifd)?);
        (0..n)
            .map(|k| ifd + 2 + 12 * k)
            .find(|&e| u16_at(b, e) == Some(tag))
    };
    let Some(ifd0) = u32_at(exif, 4) else { return };
    let Some(ptr) = find_entry(exif, ifd0, EXIF_IFD_POINTER).and_then(|e| u32_at(exif, e + 8))
    else {
        return;
    };
    let Some(e) = find_entry(exif, ptr, COLOR_SPACE) else {
        return;
    };
    if u16_at(exif, e + 8) == Some(COLOR_SPACE_SRGB) && e + 10 <= exif.len() {
        let v = if be {
            COLOR_SPACE_UNCALIBRATED.to_be_bytes()
        } else {
            COLOR_SPACE_UNCALIBRATED.to_le_bytes()
        };
        exif[e + 8..e + 10].copy_from_slice(&v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 3x2 image, one channel, values = index.
    fn grid() -> Vec<u8> {
        (0..6).collect()
    }

    #[test]
    fn all_orientations_match_exif_definitions() {
        // Source:     Displayed (orientation 6, rotate 90 CW):
        //  0 1 2       3 0
        //  3 4 5       4 1
        //              5 2
        let cases: [(u8, &[u8], u32); 8] = [
            (1, &[0, 1, 2, 3, 4, 5], 3),
            (2, &[2, 1, 0, 5, 4, 3], 3),
            (3, &[5, 4, 3, 2, 1, 0], 3),
            (4, &[3, 4, 5, 0, 1, 2], 3),
            (5, &[0, 3, 1, 4, 2, 5], 2),
            (6, &[3, 0, 4, 1, 5, 2], 2),
            (7, &[5, 2, 4, 1, 3, 0], 2),
            (8, &[2, 5, 1, 4, 0, 3], 2),
        ];
        for (o, want, w) in cases {
            let (out, ow, _) = apply(&grid(), 3, 2, 1, o);
            assert_eq!((out.as_slice(), ow), (want, w), "orientation {o}");
        }
    }

    #[test]
    fn xmp_orientation_reset_both_forms() {
        let mut a = br#"<x tiff:Orientation="6" tiff:Orientation2="8"/>"#.to_vec();
        reset_xmp_orientation(&mut a);
        assert_eq!(a, br#"<x tiff:Orientation="1" tiff:Orientation2="8"/>"#);
        let mut b = b"<tiff:Orientation>8</tiff:Orientation>".to_vec();
        reset_xmp_orientation(&mut b);
        assert_eq!(b, b"<tiff:Orientation>1</tiff:Orientation>");
    }

    #[test]
    fn exif_color_space_marked_uncalibrated() {
        // LE TIFF: IFD0 {ExifIFD -> 26}, Exif IFD {ColorSpace = 1}.
        let mut t = b"II*\0".to_vec();
        t.extend_from_slice(&8u32.to_le_bytes());
        t.extend_from_slice(&1u16.to_le_bytes());
        t.extend_from_slice(&EXIF_IFD_POINTER.to_le_bytes());
        t.extend_from_slice(&4u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&26u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&1u16.to_le_bytes());
        t.extend_from_slice(&COLOR_SPACE.to_le_bytes());
        t.extend_from_slice(&3u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&1u16.to_le_bytes());
        t.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        mark_exif_uncalibrated(&mut t);
        assert_eq!(&t[26 + 2 + 8..26 + 2 + 10], &[0xFF, 0xFF]);
        mark_exif_uncalibrated(&mut t[..20]); // truncated: no panic
    }
}
