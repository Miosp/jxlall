//! Color description: CICP preferred, ICC fallback.

/// Coding-independent code points (primaries + transfer), per CICP/CICP-like
/// sources (HEIF `nclx`, PNG `cICP`).
#[derive(Debug, Default, Clone)]
pub struct Cicp {
    pub color_primaries: u16,
    pub transfer_characteristics: u16,
}

/// Resolved color description of a decoded image.
#[derive(Default)]
pub struct ColorDesc {
    pub cicp: Option<Cicp>,
    pub icc: Option<Vec<u8>>,
}

impl ColorDesc {
    pub fn new(cicp: Option<Cicp>, icc: Option<Vec<u8>>) -> Self {
        Self { cicp, icc }
    }
}
