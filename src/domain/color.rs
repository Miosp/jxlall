//! Color description: CICP preferred, ICC fallback.

/// Coding-independent code points (primaries + transfer), per CICP/CICP-like
/// sources (HEIF `nclx`, PNG `cICP`).
#[derive(Debug, Default, Clone)]
pub struct Cicp {
    pub color_primaries: u16,
    pub transfer_characteristics: u16,
}

/// Explicit gamma and/or chromaticities (PNG `gAMA`/`cHRM`), used only
/// when the source has neither CICP nor ICC.
#[derive(Debug, Default, Clone)]
pub struct CustomColor {
    /// OETF exponent (linear → encoded), e.g. 0.45455 for gamma 2.2.
    pub gamma: Option<f64>,
    /// CIE xy of white, red, green, blue.
    pub chromaticities: Option<[[f64; 2]; 4]>,
}

/// ICC data color space (header bytes 16..20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IccSpace {
    Rgb,
    Gray,
    /// CMYK, Lab, ...: cannot describe the RGB pixels we produce.
    Other,
}

/// Resolved color description of a decoded image.
#[derive(Default)]
pub struct ColorDesc {
    pub cicp: Option<Cicp>,
    pub icc: Option<Vec<u8>>,
    pub custom: Option<CustomColor>,
}

impl ColorDesc {
    pub fn new(cicp: Option<Cicp>, icc: Option<Vec<u8>>) -> Self {
        let icc = match icc {
            Some(p) if icc_header_ok(&p) => Some(p),
            Some(_) => {
                eprintln!("note: malformed ICC profile dropped, encoding with default color");
                None
            }
            None => None,
        };
        Self {
            cicp,
            icc,
            custom: None,
        }
    }

    pub fn custom(mut self, c: Option<CustomColor>) -> Self {
        self.custom = c;
        self
    }

    /// Color space of the (header-validated) ICC profile, if any.
    pub fn icc_space(&self) -> Option<IccSpace> {
        self.icc.as_deref().map(|p| match &p[16..20] {
            b"RGB " => IccSpace::Rgb,
            b"GRAY" => IccSpace::Gray,
            _ => IccSpace::Other,
        })
    }
}

/// Minimal ICC profile-header sanity check: a profile whose declared size or
/// `acsp` signature is wrong would be rejected by the JXL encoder, failing
/// the whole file. Dropping it degrades color accuracy instead of killing the
/// conversion.
fn icc_header_ok(p: &[u8]) -> bool {
    p.len() >= 128
        && u32::from_be_bytes([p[0], p[1], p[2], p[3]]) <= p.len() as u32
        && p.len() - u32::from_be_bytes([p[0], p[1], p[2], p[3]]) as usize <= 4
        && p[36..40] == *b"acsp"
}
