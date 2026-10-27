//! Input format detection by magic bytes.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Tiff,
    WebP,
    /// HEIF-family container: heic/heix/hevc/heim/heis/hevm/hevs/mif1/msf1/mif3/mif2/avif/avis.
    Heif,
}

/// Sniff format from leading bytes. Returns `None` for unknown formats.
pub fn sniff(data: &[u8]) -> Option<Format> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Format::Jpeg);
    }
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(Format::Png);
    }
    const TIFF_MAGIC: [&[u8]; 4] = [b"II*\x00", b"MM\x00*", b"II+\x00", b"MM\x00+"];
    if TIFF_MAGIC.iter().any(|m| data.starts_with(m)) {
        return Some(Format::Tiff);
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Some(Format::WebP);
    }
    // ISOBMFF: size(4) + "ftyp" + major brand(4) + minor version(4) + compatible brands(4 each)
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        const BRANDS: [&[u8; 4]; 13] = [
            b"heic", b"heix", b"hevc", b"heim", b"heis", b"hevm", b"hevs", b"mif1", b"msf1",
            b"mif3", b"mif2", b"avif", b"avis",
        ];
        const COMPATIBLE_BRANDS_START: usize = 16;
        let box_len = u32::from_be_bytes(data[0..4].try_into().ok()?) as usize;
        let ftyp = &data[..box_len.min(data.len())];
        let compatible = ftyp.get(COMPATIBLE_BRANDS_START..).unwrap_or_default();
        let major: &[u8; 4] = data[8..12].try_into().ok()?;
        let mut brands = std::iter::once(major).chain(compatible.as_chunks::<4>().0);
        if brands.any(|b| BRANDS.contains(&b)) {
            return Some(Format::Heif);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bigtiff_magic() {
        assert_eq!(sniff(b"II+\x00\x08\x00\x00\x00"), Some(Format::Tiff));
        assert_eq!(sniff(b"MM\x00+\x00\x08\x00\x00"), Some(Format::Tiff));
    }

    #[test]
    fn heif_compatible_brand() {
        let mut ftyp = 24u32.to_be_bytes().to_vec();
        ftyp.extend_from_slice(b"ftypxxxx\0\0\0\0isommif1");
        assert_eq!(sniff(&ftyp), Some(Format::Heif));
    }

    #[test]
    fn brands_outside_ftyp_box_ignored() {
        let mut data = 16u32.to_be_bytes().to_vec();
        data.extend_from_slice(b"ftypxxxx\0\0\0\0mif1");
        assert_eq!(sniff(&data), None);
        // Video MP4 (no image brand) stays unrecognized.
        let mut mp4 = 24u32.to_be_bytes().to_vec();
        mp4.extend_from_slice(b"ftypisom\0\0\0\0isomavc1");
        assert_eq!(sniff(&mp4), None);
    }
}
