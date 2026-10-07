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
    if data.starts_with(b"II*\x00") || data.starts_with(b"MM\x00*") {
        return Some(Format::Tiff);
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Some(Format::WebP);
    }
    // ISOBMFF: size(4) + "ftyp" + major brand(4)
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        const BRANDS: [&[u8; 4]; 13] = [
            b"heic", b"heix", b"hevc", b"heim", b"heis", b"hevm", b"hevs", b"mif1", b"msf1",
            b"mif3", b"mif2", b"avif", b"avis",
        ];
        let brand: &[u8; 4] = data[8..12].try_into().ok()?;
        if BRANDS.contains(&brand) {
            return Some(Format::Heif);
        }
    }
    None
}
